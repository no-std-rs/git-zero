# R11 peak-in-catalog candidate

Clean `7b6f626` integrates as `26e3d61` with identical example source. Independent
source review found no remaining blocker after two fixes: malformed GC keys reject
before migration/deletion, and incomplete retirement forces Native reconciliation
before its next request while preserving the published push's success.

A new registration capability gates the peak-column upgrade for fresh clients.
PUT stores the historical aggregate peak in its existing catalog row and syncs
before CAS. Last-witness deletion and GC preserve a durable monotone fallback
before deleting objects. Existing v2 repositories keep these rules after registry
loss or an older Native reconnect. The documented R2/SQL crash boundary still
requires Native startup/ambiguous-result inventory reconciliation.

Actual normal PUT/three-key retirement changes SQL 9 reads/6 writes to 8 reads/5
writes, with identical one PUT, three DELETEs, 17 written bytes and one Worker/DO
callback. Realistic 11/13-key, 33-ref, 11-pack shapes remain **24,576 SQLite bytes**.
The first eligible ALTER actually costs six reads and one write, charged to that
mutation. These are correctness controls, not four-profile benchmark results.

Validation includes 110 package tests, strict Clippy/fmt, actual v1/v2 fault and
migration contracts, seven cases across three Wrangler processes, and a real
five-push next-request recovery regression (failing before the fix, passing after).
The root's additional production-Worker R10/R11 driver passes historical reads
with/without haves, partial views, cache hits, empty probes, default large push,
strict fsck/archive and charged final full restoration. All owned runtimes stopped.

`validation.txt` preserves commands, scope, retries and crash limits. Compressed
logs include failures and the existing randomized SHA-mutation test correction;
no production advertisement policy changed. Shape/restart/combined JSON retain
actual cursors and counters. No speed, complete bill or benchmark acceptance is
claimed. Worker CPU, DO duration, monthly SQL storage and native compute are unknown.
