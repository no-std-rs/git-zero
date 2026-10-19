# Object formats and decoding

## Object formats

`ObjectFormat::{Sha1, Sha256}` selects one fixed format for a repository.
`ObjectId::{Sha1([u8; 20]), Sha256([u8; 32])}` preserves that format and all ID
bytes. Replace the former tuple constructor with `ObjectId::Sha1(bytes)`, and
replace `.0` with `as_bytes()`. `from_hex` accepts exactly 40 or 64 hex digits;
`to_hex()` returns fixed storage exposing only those active bytes.

Existing snapshot constructors and `Object::id()` default to SHA-1 and retain
their object bytes and IDs. Use `Snapshot::new_with_format`,
`Snapshot::from_entries_with_format` or `SnapshotBuilder::finish_with_format`
for SHA-256; corresponding `*_with_commit_and_format` methods accept custom
metadata. Tree IDs occupy 32 binary bytes and commit/tag links use 64 hex digits.
Tree storage therefore needs another 12 bytes per entry compared with SHA-1.
Empty repositories use `EmptyRepository::new_with_format`.

Custom SHA-256 providers must override `Repository::object_format()` (its default
is SHA-1), return IDs in that format, and use its zero ID for unborn HEAD.
Indexes and validated/ref wrappers forward the provider's format. Validation
rejects mixed IDs, wrong-width links and hash mismatches. `FileRepository` takes
its format from the supplied HEAD ID, including an unborn zero ID.

Smart v0 discovery advertises `object-format=sha1` or `object-format=sha256`.
The first want may explicitly select that format; mismatches and unadvertised
capabilities fail before pack output. [Git's v0 client](https://github.com/git/git/blob/v2.53.0/fetch-pack.c)
selects the advertised
format without repeating it on wants, so omission also uses the repository's
format. Every want/have still requires the correct ID width. Dumb refs and loose
paths use 40 or 64 digits. Independent real-Git clone/fetch tests cover both.
Client discovery rejects mixed ID formats and capabilities that conflict with
the advertised IDs, including the empty-repository marker.
Packs use a 20-byte SHA-1 or 32-byte SHA-256 trailer; HEAD cache keys include the
ID's format. This does not translate objects between formats.

## Incremental zlib inflation

`zlib::Inflater` decodes downloaded zlib bytes without allocation or external
compression dependencies. It borrows one caller-owned `zlib::WINDOW_SIZE`
(32 KiB) history array; its resumable state is less than 2 KiB. Input and output
chunks can be as small as one byte, and overlapping back-references work across
history wraps and block boundaries.

Construct the decoder with `InflateLimits { max_output, max_ratio, ratio_slack }`.
Before each output byte it enforces both the absolute output cap and
`output <= consumed_input * max_ratio + ratio_slack`, using saturating arithmetic.
Only consumed bytes from the current zlib stream contribute to the ratio;
unconsumed trailing bytes cannot enlarge the budget. The explicit slack permits
compressible prefixes. Zero ratio permits at most the slack bytes. These checks
behave identically across chunk partitions.

`update(input, output, end_of_input)` returns consumed/written counts and
`NeedInput`, `NeedOutput` or `Finished`. Advance input by the consumed count,
retain the decoder and provide the unconsumed suffix on the next call. Set
`end_of_input` when the supplied slice contains the last available bytes;
missing input then reports `InvalidZlib`, while full output remains resumable.
Empty chunks are accepted, and exact output budgets allow parsing the final
block and checksum. `Finished` leaves subsequent pack entries or trailing bytes
untouched; callers requiring one complete loose-object response must reject any
remaining suffix.

Stage output until `Finished`, which validates the zlib header, block structure,
back-reference distances and Adler-32. Discard staged data on error; a failing
call can modify its output buffer. Malformed/truncated data returns
`InvalidZlib`, exhausted expansion budgets return `LimitExceeded`, and preset
dictionaries return `Unsupported`. Errors are terminal until `reset(limits)`.
The standalone inflater leaves Git envelope validation to the object decoder.
See the [API example](../crates/git-zero-core/src/zlib.rs) for a complete bounded loop.

## Validated client objects and pack entries

`client::decode_loose(compressed, expected_id, staging, window, limits)` validates
one complete loose response. Its staging capacity and inflation limits include
the Git header and body. It returns a borrowed `Object` only after the zlib
checksum, canonical type/unsigned decimal size, exact body length and expected
hash in the ID's format match. Concatenated streams and trailing bytes fail. `fetch_loose_object`
combines the existing dumb transport request with this decoding, using separate
compressed/inflated buffers. `Object::from_loose` performs envelope/ID checks on
bytes already inflated by a host. These checks do not validate commit/tree/tag
semantics or traverse the repository graph.

`pack::Reader::new(pack)` checks a complete SHA-1 pack's header and trailer.
Use `Reader::new_with_format(pack, ObjectFormat::Sha256)` and
`pack::validate_with_format` for SHA-256 packs.
`read_entry(staging, window, limits)` decodes one entry into caller buffers,
checks its declared size and records offsets and the wire CRC-32. A failure
preserves the cursor for retry; discard modified staging bytes. `finish()` or a
read returning `None` verifies the declared entry count and exact pack closure.
`Entry::verify_crc32` compares a trusted index CRC with the computed CRC; CRCs
are carried by Git's index files rather than the pack entries themselves.

Plain entries expose `Entry::object()` as a `ResolvedObject` with a computed
canonical ID. For deltas, provide a materialized base to `resolve_delta` plus
separate result storage and `DeltaLimits { max_output, max_instructions,
max_depth }`. Offset bases must have the correct offset in the same pack;
reference bases must have the advertised ID. Host objects can become reference
bases through `ResolvedObject::external` (SHA-1) or `external_with_format`.
Delta copies, inserts, base/result
sizes, exact instruction closure and chain depth are checked before returning
a canonical body and ID. `verify_id` compares it with a trusted expected ID.
`pack::apply_delta` provides the one-level instruction primitive separately.

The reader needs the complete compressed pack plus staging for one inflated
body/instruction stream and one 32 KiB history window. Delta application also
needs its canonical base and caller-bounded result storage. No recursion or
allocation occurs. The native `fetch` command retains its wire-output behavior.

`pack::materialize(pack, index, arena, scratch, window, limits)` resolves a
complete non-thin SHA-1 pack, including forward reference deltas, into a
`MaterializedPack`. The host supplies metadata slots and a byte arena; neither
the compressed pack nor scratch is retained. Lookup uses the sorted object ID
index. Inflation, delta depth/instructions, pass count and decoded bytes per
pass, delta-base metadata probes, and total tree-validation reads are bounded by
`MaterializeLimits`. Index sorting uses an iterative heap sort with constant
stack space. Failed imports invalidate partial storage.
This checks pack/object/tree structure; graph closure is a separate operation.

`object::Commit::parse` exposes the root tree and exact message bytes.
`object::Tree::parse` checks modes, byte-string names, ordering and duplicates,
then exposes borrowed entries. `Tree::parse_with_budget` bounds reads and alias
rescans; `Tree::parse` uses a 64 MiB validation budget. `object::Tag::parse` extracts an annotated tag's
target and declared kind. Commit and tree parsers default to SHA-1; their
`*_with_format` methods accept SHA-256, and `Tree::parse_with_budget_and_format`
combines the format with a caller-owned read budget. Tags infer their target ID
format from its hexadecimal width. Host filesystem limits stay in the adapters; use
`ValidatedRepository` for stronger graph and reference validation.

`client::SmartAdvertisement::parse` and `read_smart_reference` handle a downloaded
protocol-v0 advertisement without I/O. Its `fetch_options` selects advertised
capabilities and `validate_want` checks permission to request an explicit ID.
`write_fetch_request` deduplicates wanted IDs and encodes optional shallow/offset-delta negotiation.
SHA-256 wants include their object-format capability; mixed-format wants are
rejected before emitting output. `write_fetch_request_iter` accepts a cloneable
iterator so adapters can retain their own ID representation.
`fetch_pack_with_options` combines this with the host transport, and
`read_pack_response` handles shallow negotiation before authenticating a raw
pack; `read_pack_response_with_format` checks SHA-256 boundaries and checksums.
All these APIs use caller-owned storage without `std`, `alloc` or unsafe
code.
