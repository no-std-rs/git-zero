# Reviewed batch02: two completed volatile local diagnostics

Both cases completed on exact SOURCE `e41159b7fcd3fc9ef16b189464fd6c54189af13b`, actual supervisor
ELF `b36f7564ca6b83f4d316f617b0eaae728028296caf3f9020f4fac315362ad182`, without source/ELF edit, build, retry or repeat.
The batch ran `2026-10-18T13:53:03.916143+00:00` through `2026-10-18T14:01:14.235926+00:00`
(490.319810s diagnostic wall time). Each used the
reviewed1200s work plus separate fixed5s cleanup grace, exited0 and retained
complete original prefix/order/observer receipts. The interrupted chat did not
start another run; all subsequent work was read-only collection and sealing.

**The earlier severe tails did not reproduce. No latency fix or cause is proven.**
The private raw filesystem is volatile local tmpfs; current R19+approved hooks
is distinct from historical R18 on GitHub VM disk. These are instrumented
single-replica observations, without matched standard-VM controls or a new
uninstrumented cohort. They provide no speed, cost, benchmark acceptance or
power-loss durability claim. CPU, Worker/DO duration and full bill stay NULL.

| Case | Fixture/ops | Target client gateway wall | Worst gateway operation | Complete case elapsed |
| --- | --- | ---: | --- | ---: |
| cold64 target16 | all32 generations,42ops | 4698.999127ms | same target4698.999127ms | 324149ms |
| warm16 target32 | all32 generations,86ops | 103.215742ms | push09,847.461840ms | 165397ms |

The sealed historical R18 target rows were173087.366597ms and39376.952628ms,
respectively. They remain separate historical context: no ratio, source reuse
or causal attribution follows from this different local environment.

## Target boundaries and worst actual operations

All phase values below are elapsed observations, **not CPU**. Native stages
share each Native's monotonic clock; uncertainty in its boot calibration is
10,037,721ns cold and10,049,503ns warm. Client start ticks have10ms resolution
plus spawn/setup placement uncertainty; exact cross-process alignment is not
claimed. The complete stage lists are in `phase-timelines.json`. Historical packet traces
were not retained: only current request/body hashes and logical fixture/order
are verified; exact old request bytes are not claimed. Mapping uses
unchanged sequential before/after metrics boundaries, without extra service
observations or correlation headers. Nested spans overlap.

| Case | Operation | Client command wall ms | Largest Native HTTP span ms | Its handler ms | Largest Native Git child span ms |
| --- | --- | ---: | ---: | ---: | ---: |
| 64mib-cold-0 | history_no_haves_16 | 4698.999 | 4278.822 | 2709.670 | 618.984 |
| 64mib-cold-0 | push_14 | 3345.464 | 3150.036 | 2525.143 | 329.139 |
| 64mib-cold-0 | push_09 | 2885.608 | 2629.551 | 2391.784 | 111.237 |
| 16mib-warm-0 | push_09 | 847.462 | 815.385 | 700.086 | 72.409 |
| 16mib-warm-0 | push_17 | 813.001 | 784.680 | 671.244 | 60.281 |
| 16mib-warm-0 | push_13 | 778.157 | 752.390 | 640.557 | 58.665 |
| 16mib-warm-0 | history_no_haves_32 | 103.216 | 34.234 | 21.276 | 20.305 |

Cold target16 has three Native requests: advertisement1.161114ms,
`fetch-plan`1.603392ms and internal `fetch-continue`4278.822222ms. For that
continuation: header0.081563ms, internal body read178.889786ms,
post-body/pre-handler interval924.155084ms, handler2709.670104ms,
response header0.044403ms and response body write457.662103ms. The internal
body is16,778,438B; its logical Git request is separately recorded as183B with
its own hash. The source places diagnostic body hashing/bookkeeping before
handler_begin; the924ms interval includes that work and scheduling, and does
not isolate its CPU cost. The materialize span is549.271149ms; Native Git
upload-pack span618.983709ms; native_read665.728218ms. These nested durations
are not additive. The reused WebSocket blobs/read callback is110.225188ms
(transport span109.214566ms), response8,778,750B. There is no new-connect timing
for that reused call. Recorded operation deltas show3DO requests,3R2 class-B
operations and25,555,938B R2 reads. Worker-internal prefetch timing is not
measured by the Native callback span. Native upload_codec elapsed delta is
0.666766692s; its scope does not cover the whole client/HTTP interval.

Warm target32 has advertisement0.211005ms and upload HTTP34.233700ms. Upload
body read0.006412ms, post-body/pre-handler0.020408ms, handler21.276174ms and
response body write12.796203ms. View preparation is0.011742ms, Native Git
upload-pack20.304815ms and native_read21.057004ms. There is no Native service
callback during the target. Actual service counters still show **2DO requests,
2rows read**, with0R2 operations/read bytes; this is not free/unobserved work.
Worker-internal DO timing remains unknown. Client wait/drain elapsed is a
separate scope from these Native stages.

No gateway operation exceeded4.699s. The full128-operation journal, including
all prefix events, is preserved. The selected top3 operations per case plus
both targets have detailed request/callback/Git/view stage lists and byte/hash
receipts. There was no severe new request requiring a retry or protocol change.

## Strict correctness, scope and runtime audit

Both complete32-generation source reconstructions match every sealed want and
tree before services started. Prefixes match all42/86 sealed operations exactly.
All26+54 newly fetched packs have successful explicit strict-index command/
status/hash receipts, exact wants/trees, strict fsck and matching client/source
archive hashes. Persistent retained pack files rehash; fresh-client packs were
removed in ordinary frozen order, with their original pre-removal hash/index
receipts retained. No pack or archive was regenerated during collection.

509cold and837warm commands have successful status/PGID/stream receipts. Full
hashes were recomputed for966+1566 fully retained command streams;52+108 large
archive streams retain only prefixes, so their full hashes are original
streaming receipts, not independently reconstructed here. All390 actual HTTP
observation bodies/headers/statuses verify. Integer counters compare exactly;
small float reserialization differences in elapsed counters are recorded
(max1.78e-15cold/2.22e-16warm, tolerance1e-12), without rewriting raw bodies.
Both12-counter startup/ordinary/observer closure identities recompute. All1596
Native records complete, with no error/truncation;783 request-hash records match
those records. CPU and full bill remain unknown rather than zero/free.

The exact authconfig equals the raw approved config; case inputs differ from
the sealed plans only by the four reviewed execution overlays. Both source/
runtime before/after sets agree, all50 references and actual runtime maps
rehash, and the worktree remains clean at e41159b7. Native ELF remains
`3ccf43bb5cca872294191fef57df9d4469ddd5dad41e9b5abf4cd6e270d1f060`;
stage-one runner remains
`d2e21856771f7fb3b8bb0e15517c4ab150645dc623255880d90699663a1db943`.
The frozen R19 base is f124d3b71ce1acc44ab20d6850b2b619b4bd8ec5, with approved
hooks a827df032e87374737b1ac7e36fb518b507e8411 and exact common Worker modules.

## Actual cold environment capture remains incomplete

Cold's original `native-environment.json` exists with no recorded error and
`git_environment_names=[]`. Its eligibility=true is only the empty invalid-name
check and is **not accepted as canonical-environment proof**. Source order
means `/proc/2054793/environ` was read successfully before saving the file;
raw bytes and byte count were not retained, so an empty raw read versus another
transient state is unresolvable after exit. It was saved at13:53:30.812926Z,
39ms before the captured readiness output, for owned PID2054793, start tick
56914601, PGID2048607. Before/after live maps establish that same Native PID,
generation, PGID and actual ELF through ordinary TERM/reaping. A startup exec
transition is possible, not proven. **Actual cold canonical environment is NULL.**

Cold actual applicable counters independently show1advertisement certificate,
15shortcuts,23consumed read preparations of26plans,12consumed receive plans and
7history projections. They establish their observed behavior only and do not
replace the missing environment proof. Warm's capture contains the expected
four canonical settings plus GIT_ZERO_PHASE_LEDGER; its actual-name receipt is
complete. Warm uses the approved persistent-view policy, where cold-only
certificate/preparation/lineage counters are0. No parent Trace2 was injected by
the executor. Cold's missing actual environment receipt cannot certify absent
unknown variables retroactively. No rerun or source fix was performed.

## Owned cleanup, raw inventory and compact mirror

Both ordinary Native TERM and Node143 receipts show reaping and complete drains;
both case leaders exit0, joins complete and supervisor-owned groups are empty.
Collection checked1746 exact recorded PID/start-time generations and owned
8853/8854 listener entries: none remained. No signal, cleanup mutation,
unrelated process/environment inspection or new runtime was needed.

All77+121 supervised resource boundaries pass against the actual raw tmpfs
mount, unchanged2GiB reserve/8GiB RAM floor and8GiB cumulative sampled cap.
The final raw inventory covers8519 regular files,480,489,172 logical bytes
(499,834,880 unique regular-file allocated bytes, excluding directory overhead).
The compact independent mirror holds6929 files/11,770,501B. Raw packs, working
trees, emulator SQLite/WAL/tool state and large archive prefixes stay in the
private volatile raw prefix; all have full artifact hashes in the raw inventory.
This mirror is a metadata/phase proof copy, not a durable backup of all raw data.

- `raw-inventory.jsonl` SHA256 `ef8a8d6238aa25dd186e85130ca19fffa21a5c0ee1aaea4b68b68010f6c9d164`.
- `raw-files.sha256` SHA256 `e26fbb760bb8702fb70d48176fbaadf9f5b18b801ae24ce3cecc8efc964c0fa5`.
- `mirror-relocation-map.json` SHA256 `5d4e8c808fa06c1f627fbedc10893d4e4a59b3b1ceb0dd2bfa1dd29019f2b752`.

Original raw contents, blocked disk01 and prior proofs/reviews/archives remain
unchanged. Original filesystem timestamps are in the inventory/map; mirror
mtimes do not claim to be original event times. Read-only raw access requires
host escalation because the ordinary sandbox hides /dev/shm.

STOP for independent result/scope review. These non-reproducing cases establish
no production latency fix. The reviewer can assess the cold capture gap and
whether a separately authorized standard GitHub VM diagnostic is the next
useful step. No further case, source edit/build, CI/deploy/push, main work,
mandatory final-five run or delegation occurred or is authorized here.
