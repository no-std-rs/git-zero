# Final-upload publication candidate: correctness validation

This candidate was implemented in the isolated `perf/hosting-publish-upload`
worktree from `640b144ced0fd4d8fc0033472a66ce960cec0318`. The exact implementation
revision is the Git commit containing this record. It does not include the
independent native advertisement shortcut, which the parent branch integrates
first. No benchmark, paid deployment, remote timing, or candidate cost/latency
claim was made in this implementation worktree.

The cloud importer now stages only its final new content-addressed chunk. A
private immutable `PendingChunk` proves its descriptor from the exact owned
bytes once; it cannot be constructed or mutated by an external adapter. One
bounded scratch buffer builds each virtual pack/index chunk. Prior pending tails
are flushed only before a subsequent new pack. Eager blob and pack APIs and local
native hard links keep their durable contracts.

An exact registration capability selects the GZUP v1 binary request. The complete
frame is at most 16 MiB + 2,060 bytes, metadata at most 1,500,000 bytes, payload at
most 16 MiB, retirement at most 256 keys, and strict replies at most 32 KiB. A
pre-I/O capability or frame/debt bound fallback performs one ordinary PUT then
the existing retiring publication. Errors after a fused attempt never retry PUT
or CAS. The native view becomes dirty before staging and recovers authoritative
state after ambiguous errors. Snapshot, request, cache and compaction limits are
unchanged.

The Worker validates the complete bounded frame before mutation and invokes the
same actual PUT/catalog/inventory/high-water helper plus storage sync, then the
existing transactional CAS plus sync, then sequential retirement plus final
sync. No new SQL schema, pending payload table, HEAD, generation read or skipped
binding operation was added. Stale uploads remain durable until authoritative
GC decides whether they are shared live keys or orphans.

## Completed checks

- `cargo fmt --all --check`, `git diff --check`, and Node syntax checks for the
  production Worker and the test-only fault wrapper passed.
- `CARGO_TARGET_DIR=/tmp/git-zero-publish-upload-target cargo clippy -p
  git-zero-host --all-targets -j2 -- -D warnings` passed.
- `CARGO_TARGET_DIR=/tmp/git-zero-publish-upload-target cargo test -p
  git-zero-host -j2` passed: 74 library, 3 Rust runner, 9 hosting and 1 storage
  lifecycle test. Opt-in workerd contracts remain ignored in this default run.
- All 16 `real_` Rust workerd contracts passed on the production Worker config,
  including registered native push/fetch/compaction/archive/fsck, strict framing,
  full chunk bounds, repeated reads and idle DO eviction accounting.
- Two upload fault contracts passed using only the test-only binding/barrier
  wrapper: real failing R2 PUT/DELETE arguments, post-PUT catalog interruption,
  first upload barrier failure, CAS barrier failure, partial deletion, corrupt
  and lost committed replies, and native authoritative recovery without another
  PUT/CAS.
- Both phases of the new mixed-layout upload restart proof passed in separate
  actual Wrangler processes with the same persisted state. It contains old
  8 MiB chunks, a new 16 MiB-layout fused tail, a stale-upload orphan, incomplete
  durable retirement debt and a shared live key. Verification checks runtime
  epoch change, complete pack/index hashes, Git fsck, objects, shared keys and GC.
- Both phases of a separate catalog-interruption restart proof passed. A real
  PUT interrupted before catalog upsert survives the process restart and is
  removed by authoritative startup listing/reconciliation without publication.

The focused operation contract compares eager PUT plus fused retirement with
staged upload/publication/retirement. It observes exactly **2 versus 1 Worker
service requests and DO requests**, with **1 actual R2 PUT, 3 actual deletes,
17 written bytes, zero reads, and identical SQL read/write rows** in both paths.
This is a correctness/accounting contract, not a workload benchmark or latency
measurement. A full-tail frame overflow separately confirms exactly one ordinary
PUT and one retiring-publication request before successful fallback.

The invalid-frame contract includes truncated header/metadata/payload, trailing
bytes, bad magic/version, unsafe generations, invalid UTF-8/JSON, decoded duplicate
and unknown fields, missing fields, bad hash, membership/length disagreement,
mixed pack representations, debt overflow, metadata overflow and wire overflow.
Every rejected frame leaves PUT/CAS/DELETE operations unchanged. It also accepts
exactly 1,500,000 metadata bytes and exactly 16 MiB + 2,060 total wire bytes.

## Exact local commands

All commands ran from `/tmp/git-zero-hosting-publish-upload`. Runtime dependency
reuse was local; no MJS runner or deployment was used. Tool versions were Rust
and Cargo 1.97.1, Node v24.10.0, Git 2.53.0 and Wrangler 4.149.0.

Start the production configuration (port 8827 was exclusive to this task):

```sh
WRANGLER_LOG_PATH=/tmp/git-zero-publish-upload-wrangler-final.log \
WRANGLER_SEND_METRICS=false \
node /tmp/git-zero-hosting-fused-retirement/examples/repository-host/cloudflare/node_modules/wrangler/bin/wrangler.js \
  dev --local --ip 127.0.0.1 --port 8827 --inspector-port 0 \
  --show-interactive-dev-session=false \
  --config /tmp/git-zero-hosting-publish-upload/examples/repository-host/cloudflare/wrangler.toml \
  --persist-to /tmp/git-zero-publish-upload-final-state
```

Run real contracts with:

```sh
GIT_ZERO_CLOUDFLARE_ORIGIN=http://127.0.0.1:8827 \
GIT_ZERO_CLOUDFLARE_TOKEN=git-zero-local-services \
CARGO_TARGET_DIR=/tmp/git-zero-publish-upload-target \
cargo test -p git-zero-host --test cloudflare real_ -j2 \
  -- --ignored --test-threads=1
```

For fault contracts, stop this runtime, start Wrangler with
`--config /tmp/git-zero-hosting-publish-upload/examples/repository-host/cloudflare/test/upload-fault.toml`
and `--persist-to /tmp/git-zero-publish-upload-fault-state`, then use the same
Rust test command with each filter `faulted_upload_barriers` and
`faulted_native_upload_lost_response`.

The production-config two-phase proof used
`--persist-to /tmp/git-zero-publish-upload-state`,
`GIT_ZERO_CLOUDFLARE_RESTART_REPO=upload-restart-20261018`, and separate Rust
invocations filtered by `seed_upload_before_workerd_restart` and
`verify_upload_after_workerd_restart`, with `-- --ignored --exact
--test-threads=1` and an actual Wrangler stop/start between them.

The catalog proof used the upload-fault config,
`--persist-to /tmp/git-zero-publish-upload-catalog-restart-state`,
`GIT_ZERO_CLOUDFLARE_RESTART_REPO=contract-upload-catalog-restart-20261018`, and
filters `seed_upload_catalog_before_workerd_restart` and
`verify_upload_catalog_after_workerd_restart`, again with an actual process
stop/start and the same explicit test flags.

Production Worker SHA-256:
`83b52ba0eabd9f752aa1c243ce285f20ab78caac9244329d00d0db788306f9bc`.

## Observational resources and unsuccessful attempts

A `/usr/bin/time -v` observation of the successful Rust native lost-reply contract
reported a maximum resident set of **51,040 KiB** for its command/process tree.
The final production-config real-contract session's `/proc` observations recorded
**98,356 KiB** high-water RSS for its gateway workerd process and **355,152 KiB**
for its separate application/emulator workerd process. An earlier development
runtime recorded 106,248 / 234,264 KiB respectively. These are local process
observations, with workerd/emulator overhead and multiple test repositories; they
are not isolate memory measurements, are not a 128 MiB isolate-fit proof, and do
not establish candidate resource or latency acceptance. Independent workload
resource gates remain required. Raw local observations are retained under
`/tmp/git-zero-publish-upload-{final-runtime-memory,runtime-memory}.json` and
`/tmp/git-zero-publish-upload-native-rss.txt`.

The first compile failed with E0689 on an untyped zero used with
`saturating_add`; annotating the offset as `usize` fixed it. The first package
unit run had 60 passing tests and 11 failures because sandboxed loopback listeners
returned EPERM; the same suite passed outside that sandbox. The initial Wrangler
start also hit sandboxed loopback EPERM and a read-only default Wrangler log path;
restarting with the explicit writable log path outside the sandbox succeeded.
The initial worktree creation hit the read-only Git metadata mount and succeeded
with the authorized isolated-worktree escalation. The first strict Clippy check
reported one `sliced_string_as_bytes` warning in a new negative test; slicing the
byte view corrected it. One intermediate edit script failed to locate formatted
text and wrote no files; the edit was reapplied and the checks above passed.
No failing correctness assertion was discarded, counter was normalized away, or
benchmark guard was weakened.
