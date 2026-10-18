# Independent reviewed-batch02 result and scope review

Decision: **VERIFIED LOCAL DIAGNOSTIC RESULTS WITH A MATERIAL COLD ENVIRONMENT LIMIT**. The retained results support completed correctness/phase diagnostics on this private local tmpfs. They do not reproduce the historical severe tails or prove a latency fix, cause, speed improvement or benchmark acceptance. Cold's actual canonical environment remains NULL, so that run does not qualify as a fully evidenced canonical cold cohort.

This review gives no new code, runtime, CI, deployment, integration or final-validation approval. Author execution was already stopped; this review performed no build, runtime, source edit, fixture generation or delegation.

## Verified execution and identities

Both original sealed cases ran once, replicas_each=1, using approved SOURCE e41159b7fcd3fc9ef16b189464fd6c54189af13b and actual supervisor ELF b36f7564ca6b83f4d316f617b0eaae728028296caf3f9020f4fac315362ad182. All 688 source blobs and 691 immutable source/ELF aliases verify; source remains clean. Native 3ccf43bb5cca872294191fef57df9d4469ddd5dad41e9b5abf4cd6e270d1f060 remains R19 f124d3b7 plus approved a827 hooks; runner d2e21856771f7fb3b8bb0e15517c4ab150645dc623255880d90699663a1db943 and common Worker tree 7f41db4407963ed04e6cbe19556b1416633caf18 remain exact. Native/hooks/Worker/dependencies/lock/workload/policies were not changed. All fifty source/runtime references and before/after runtime maps agree.

| Case | Full sealed prefix | Target gateway wall | Worst gateway operation | Case elapsed |
| --- | --- | ---: | --- | ---: |
| cold64 | 42 ops: 16 pushes, 26 reads | 4698.999127 ms | target history_no_haves_16 | 324149 ms |
| warm16 | 86 ops: 32 pushes, 54 reads | 103.215742 ms | push_09: 847.461840 ms | 165397 ms |

Each complete 32-generation reconstruction matches every sealed tip/tree before services; actual fixture-tip/tree command outputs agree. All original client/observer/operation order is preserved. All 26+54 new fetched packs have explicit successful strict-index receipts, wanted/tree checks, strict fsck and matching original full client/source archive stream hashes. All 509+837 command status/PGID/prefix receipts and 390 HTTP observations verify. Both case leaders exit zero within the original 1200-second inclusive work budget, with fixed five-second cleanup grace, no outer timeout/signals and complete drains.

Both twelve-counter scope closures recompute exactly; all 1536 per-operation integer counter identities agree. Startup, ordinary and observer/cleanup scopes remain distinct. The generic outer supervisor's counter_receipts_complete=false is not substituted for the actual successful case closure. The ledger has 1596 unique complete records and 783 matching request-hash records. Small elapsed-counter JSON float roundtrips are at most 1.776e-15 under the declared 1e-12 tolerance; integer counters compare exactly.

## Material evidence limits

Cold's original environment receipt has no Git names and eligibility=true from an empty invalid-name test. It does not establish the expected actual environment. Original raw metadata places the capture 38.999809 ms before the readiness stdout. PID 2054793, start tick 56914601, PGID 2048607 and the exact Native ELF agree across process/live/ordinary-cleanup evidence. Raw environ bytes and their byte count were not retained. A startup transition is possible, not proven. Cold canonical-environment proof stays NULL; source canonicalization and later fast-path counters cannot fill that gap. Warm retains all expected four canonical names plus its ledger variable and has complete actual-name evidence.

All 8519 raw files totaling 480,489,172 logical bytes freshly rehash, including retained prefixes and packs. All 6929 compact copies totaling 11,770,501 bytes match independent raw inodes. The raw inventory's original mode/inode/mtime/link metadata agrees. It reports 499,834,880 allocated regular-file bytes, excluding directory overhead.

Full hashes of 2532 fully retained command streams recompute. Another 160 large archive streams retain prefixes only; their full hashes are original streaming receipts whose client/source pairs agree. Forty-eight persistent client pack payloads rehash; thirty-two fresh-client pack payloads were removed in the required ordinary order and have pre-removal hash/strict-index receipts only. This review does not claim to rehash missing payloads or regenerate an archive/pack. The compact mirror is metadata/phase evidence, not a durable full raw backup. Raw tmpfs remains volatile.

Sequential Native metrics boundaries independently correlate all 128 operations. Selected top-three operations and targets match every retained request/callback/Git/view record and stage. Cold target's largest HTTP span is 4278.822222 ms for a 16,778,438-byte internal continuation body; its logical Git request is separately 183 bytes. The post-body/pre-handler interval is 924.155084 ms. Frozen source puts diagnostic body hashing/bookkeeping inside that interval; it also includes scheduling and does not isolate hash CPU. Handler is 2709.670104 ms and response-body write 457.662103 ms. Nested materialization/Git/callback spans overlap and cannot be added as exclusive phases.

Warm target's largest HTTP span is 34.233700 ms, handler 21.276174 ms. It has no Native callback during the target but actual deltas still include two DO requests and two rows read, with zero R2 bytes. Worker-internal DO/R2 timing remains unknown. Native elapsed stages are not CPU. Cross-process alignment has Native calibration uncertainty of about 10 ms plus client tick/spawn placement uncertainty. CPU, Worker/DO duration and full bill remain NULL.

## Scope, cleanup and conclusion

There was no gateway operation of ten seconds or more. Historical R18 targets of 173087.366597 and 39376.952628 ms remain historical context. Current instrumented R19+hooks on local tmpfs is a different provider/storage/host setting; these single replicas provide no matched VM-disk control, old request-byte identity, causal attribution or speed ratio.

The original disk01 launch failure is preserved: insufficient fresh disk rejected launch before any batch, fixture, helper, service or PGID. It was followed by separate root authorization for a fresh private volatile prefix. Batch02 preserves the eight-GiB raw cap, four-GiB process planning allowance, original eight-GiB available-memory floor and two-GiB raw reserve. Its stricter launch checks were twenty-four GiB available memory, ten GiB raw-mount availability and 256 MiB scratch availability. All 77+121 supervised resource boundaries passed. These are sampled checks, not filesystem/RSS quotas.

Independent read-only host checks found none of the 1746 exact recorded PID/start generations, no member of PGIDs 2048607/2146026 and no listener on 8853/8854. No signal, bind, cleanup mutation or unrelated process command-line/environment read occurred. Original approvals, blocked/source-correction history, raw receipts and manifests are preserved. The handoff manifest retains 188fad5cdb5aa21124f50977fe612fcee8ab01fe06ef4d2e517d4ea044e9b4cc; its 6949 entries verify.

## Smallest next measurement recommendation

The next useful measurement is a separately authorized standard GitHub VM with real disk running the existing two sealed prefixes once each, using the approved e411/source and actual ELFs, unchanged workload and 1200+5-second bounds. Two cases with replicas_each=1 are the minimum supported by the current reviewed batch entrypoint. Use local emulated services; no paid Cloudflare deployment is needed.

Require a fresh runner capacity/ownership decision and retain actual VM/kernel/tool/mount identities. Keep the current R19+hooks versus historical R18 distinction explicit. Any qualification of the next cold cohort requires complete actual Native environment capture after readiness, with the same owned PID/start generation and a retained raw byte count; an empty name list must not qualify. A capture/guard correction belongs to a separately reviewed diagnostic-runner change with a new frozen hash and cheap gate. No such code or runtime work is authorized here.

This measurement would test current-provider tails on standard VM disk. It would not itself prove the historical cause or justify a latency fix. Mandatory five fresh R1 and final all-four validation remain undone. Stop after this review.
