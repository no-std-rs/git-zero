# Single-verification snapshot reads: independent design

Design only, inspected clean commit `a7d06fc21b1117f8dd898b5a9517a9cee1b9aef6` in `/tmp/git-zero-hosting-cost-loop`. No repository edits, builds, tests, benchmarks, service requests, or commits were performed. No `AGENTS.md` exists in that worktree or its filesystem ancestors. The separately supplied tail diagnosis was read as context; performance observations below are its results, not new measurements.

## Proven work and narrow scope

`examples/repository-host/src/cloudflare.rs:88` decodes each bulk frame, calls `verify_chunk` at line 119, and returns untyped bytes. `HttpServices::r2_get_many` also verifies oversized scalar fallback bytes at line 475. These bytes cross `R2BlobStore::get_many` (`storage.rs:411`) and `HostBlobs::get_many` (`cloudflare.rs:557`), then `Repository::read_snapshot_files` hashes all of them again (`lib.rs:156–161`). The duplicate SHA pass is source proven.

Keep manifest validation, chunk sizes, wire framing, batch boundaries, whole-snapshot limits, ordering, repeated reads, cache behavior, storage operations, and native materialization unchanged. `read_chunks`/`read_pack_files` need no change: their scalar path already hashes once. This design does not touch StateStore/CAS/publication, avoiding the other implementation's substantive changes.

## Exact proposed API

Add this public opaque type beside `Chunk` in `storage.rs` (private fields):

```rust
pub struct VerifiedChunks {
    chunks: Vec<Chunk>,
    data: Vec<Vec<u8>>,
}

impl VerifiedChunks {
    pub fn verify(chunks: &[Chunk], data: Vec<Vec<u8>>) -> io::Result<Self>;
    pub fn into_data(self, expected: &[Chunk]) -> io::Result<Vec<Vec<u8>>>;
    pub(crate) fn append(&mut self, other: Self);
}
```

`verify` requires exactly equal item counts, calls the existing `crate::verify_chunk` for every ordered `(chunk, bytes)` pair, then owns a clone of the exact requested metadata plus the moved payload vectors. It returns no partial proof on error. The existing `verify_chunk` checks both declared length and SHA-256 content address. Do not add new size limits here; existing caller validation stays authoritative. Empty input plus empty data is a valid empty proof.

`into_data` compares `self.chunks.as_slice() == expected` before releasing bytes, returning an error on any count/key/length/order mismatch. This is an exact metadata comparison, never another SHA pass. `Chunk` already derives `PartialEq` and `Eq`. The method consumes the proof; any later bytes mutation happens after its verification authority has been consumed. No accessor returns mutable payloads. Do not derive `Default`, deserialize the type, or provide a constructor accepting arbitrary bytes without verification. A public safe verifying constructor is needed so external trait implementers can opt in.

`append` moves both ordered metadata and payload vectors from another verified proof. It does not alter any payload bytes, reorder entries, accept unverified data, or hash. This safe composition method is crate-private to keep the public API small. Initialize the HTTP accumulator with `VerifiedChunks::verify(&[], Vec::with_capacity(chunks.len()))?`; this performs zero hashes. An empty-proof capacity constructor is optional but unnecessary.

Add provided, object-safe trait methods (existing required methods/signatures remain intact):

```rust
// BlobStore
fn get_many_verified(&self, chunks: &[Chunk]) -> io::Result<VerifiedChunks> {
    VerifiedChunks::verify(chunks, self.get_many(chunks)?)
}

// CloudflareBindings
fn r2_get_many_verified(&self, chunks: &[Chunk]) -> io::Result<VerifiedChunks> {
    VerifiedChunks::verify(chunks, self.r2_get_many(chunks)?)
}
```

Old implementations, including local stores and mocks, continue to compile and get one verification before snapshot reconstruction. Defaults must call the existing batch getter, preserving custom batching/failure semantics. Do not change generic `get_many` to require content-addressed keys: the current storage contract deliberately tests `packs/abc.0`, which is valid for that generic API. The new method explicitly provides the stronger content-addressed guarantee.

## Exact forwarding and HTTP implementation

1. `R2BlobStore<T>::get_many_verified`: retain the same complete `validate_key` loop as its existing `get_many`, then return `bindings.r2_get_many_verified(chunks)` directly. Do not consume/reverify the proof here.
2. `HostBlobs::get_many_verified`: explicitly match both variants and forward to their `get_many_verified`. Omitting this wrapper override silently reinstates duplicate hashing through the trait default.
3. `HttpServices::r2_get_many_verified`: move the existing `r2_get_many` implementation here. Keep `validate_bulk_chunks(chunks)` before **any** I/O and retain `bulk_batch_len`, request bounds, paths, and sequence. Accumulate proofs. For an oversized scalar chunk, call `r2_get`, then `VerifiedChunks::verify(std::slice::from_ref(chunk), vec![data])?`, then append. For a bulk batch, append the decoder's proof.
4. Change private `decode_bulk_response` to return `io::Result<VerifiedChunks>`. Keep all count/version/magic/length/overflow/truncation/trailing/bounds checks. Collect the raw vectors, then return `VerifiedChunks::verify(chunks, result)`. Remove only its old inline `verify_chunk` call, since the safe constructor now performs that same hash exactly once. Parsing the complete frame before hashing is safe: the frame remains private and no files or bytes escape on error.
5. Keep public `HttpServices::r2_get_many` as `self.r2_get_many_verified(chunks)?.into_data(chunks)`. Thus callers of the existing public bindings API still receive an error for corruption, including scalar fallback corruption. There is no recursion: the override of the verified method contains the actual HTTP loop.
6. In `Repository::read_snapshot_files`, retain **all** existing pre-I/O representation/key/repeated-length/snapshot-bound validation. Replace `get_many`, count check, and SHA loop with `self.blobs.get_many_verified(&chunks)?.into_data(&chunks)?`. Leave reconstruction after this statement unchanged.

Verify each HTTP batch before starting the next batch. A tempting design that collects all unverified responses and verifies once at the very end would introduce extra requests following an early corrupt response; proof composition preserves existing fail-fast behavior across batches. No proof escapes a failed request. Repeated keys are retained as separate ordered entries and each returned occurrence is verified, matching current semantics.

## Integrity argument

Only the safe constructor creates a nonempty proof. It owns both metadata and bytes, eliminating mutation through caller aliases. Composition preserves their association. A buggy optional trait override may substitute a valid proof created for another request, but the repository's mandatory consuming `into_data(&chunks)` rejects any differing identity/order, even when equal-length payloads are individually valid. Equality permits repeated identical keys and lengths, as required. A corrupt payload cannot obtain a proof through any public safe API. Whole-snapshot proof consumption precedes the existing reconstruction loop and therefore every native pack/index file write. Existing `native.rs:560` obtains all snapshot files before entering its materialization loop.

## Focused implementation validation

* Opaque type: valid ordered data; too few/many payloads; wrong length; same-length corrupt bytes; wrong content key; empty proof; repeated keys; consuming against a different key, changed length, fewer items, and reversed equal-length distinct chunks. Proof identity must be exact, not a set or aggregate length.
* Legacy defaults: unchanged old-only `BlobStore` and `CloudflareBindings` test implementations compile, preserve their batch invocation, and reject corrupt/missing/count-mismatched data through the new default. Generic existing non-content-addressed `get_many` contract still passes.
* Forwarding: a test store/bindings whose old `get_many`/`r2_get_many` panics and whose verified override returns a safe proof should succeed through the repository/wrappers. This catches accidentally relying on a default and losing the optimization. A verified override deliberately returning a proof for another valid equal-length request must fail at the repository boundary. No production hash counter is needed; source audit should show one constructor/hash route and zero SHA inside `into_data`/forwarders.
* HTTP regressions: adapt existing decoder tests to consume the proof and retain all malformed frame cases. Existing direct public `r2_get_many` tests must retain corruption rejection. Add same-length corruption to bulk and oversized scalar paths; exercise typed and legacy APIs, empty requests with no RPC, repeated keys across the 256 boundary, one full 16 MiB chunk/two 8 MiB chunks, scalar-to-bulk transitions, and a corrupt first batch that prevents later requests.
* Snapshot/native: retain existing metadata-before-I/O and missing/corrupt-data tests; ensure a later corrupt pack rejects the complete snapshot before the first pack/index materialization. Preserve bundled and legacy round trips. Use existing meaningful tests and run appropriate Rust unit/integration checks; no new JS/MJS runner.

## Expected benefit and limits

For the optimized HTTP snapshot path, SHA work falls from two passes over every returned chunk to one; GET count, R2 bytes, RPC batching, and reconstructed bytes remain identical. Local/default stores retain one pass. Identity ownership adds O(number of chunks) metadata cloning and comparison, plus proof-composition vector bookkeeping; payload bytes are moved, not cloned beyond the existing decoder copy. Avoid speculative deduplication of repeated keys or unrelated SHA-formatting changes.

The supplied measurement reports 64 MiB cold small-push prepare medians of 202.902 to 230.730 ms after R2 batching; this establishes correlation, not how much this change recovers. The warm 19–76 second tails have zero R2/bulk reads and cannot be fixed by this change. The current score's known cloud dollars do not change; native SHA savings do not demonstrate lower Cloudflare billed CPU. Screen latency with the established four profiles and operation counts, then use the established final paired run if it passes; claim only observed changes. No performance or dollar win is established by this design.
