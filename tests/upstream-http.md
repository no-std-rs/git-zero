# Upstream HTTP coverage

The reference is Git **v2.53.0**:
[t5550, dumb HTTP](https://github.com/git/git/blob/v2.53.0/t/t5550-http-fetch-dumb.sh),
[t5551, smart HTTP](https://github.com/git/git/blob/v2.53.0/t/t5551-http-fetch-smart.sh),
and [the Apache harness](https://github.com/git/git/blob/v2.53.0/t/lib-httpd.sh).
Line ranges below identify the upstream assertions; descriptions group related
cases in our own words. No upstream shell bodies or GPL fixture code are copied.
Git's source and test harness are GPL-2.0; the independent Python adapters and
assertions here retain this repository's MIT OR Apache-2.0 licensing.

`upstream_http.py` uses real Git to generate each fixture, advertise refs, import
downloaded packs, enumerate reachable objects, and inspect checkout bytes/modes.
It compares git-zero with Git's oracle. It does not execute upstream shell tests
against git-zero or claim their complete Apache/client assertions pass.
`scripts/test-upstream-git.sh` remains the unchanged-suite Git reference runner.

## Supported comparisons

| Upstream cases | Feature | Independent local cases and limits |
| --- | --- | --- |
| t5550:18–76, 341–343; t5551:24–169 | Fixture publication, ref discovery, cloning and service selection | Original eight matrix cases and `upstream-refs-{smart,dumb}`. Compare every advertised ref, including peeled annotated tags and a default branch that sorts after another branch. Verify GET-only dumb traffic and smart upload-pack POSTs. |
| t5550:65–76 | Detached HEAD discovery | `upstream-detached-{smart,dumb}` uses a HEAD different from branch tips and compares it with Git. |
| t5550:85–97 | Empty SHA-1 remote | `upstream-empty-{smart,dumb}` compares empty ref lists, checks the real-Git clone, and requires git-zero fetch to return `ObjectNotFound` with no output. Empty clone/materialization in git-zero remains planned. |
| t5550:197–203, 227–243; t5551:157–169 | Changed remote, default branch and packed repository | `upstream-incremental-{smart,dumb}` changes the fixture after the initial download, checks real Git's updated remote ref/checkout, and compares git-zero's new full pack or loose HEAD. This does not test git-zero have/ACK negotiation. |
| t5550:238–243 | Packed dumb clone | `upstream-packed-dumb` checks Git's pack/index downloads. It requires git-zero's current loose-only fetch to fail explicitly. |
| t5550:271–315 | Corrupt dumb pack/index rejection | `upstream-corrupt-{pack,index}-dumb` checks Git rejects the corrupt download and removes the failed clone. These are reference baselines for future git-zero pack/index consumption. |
| t5551:639–646 | v2 discovery and clone | `upstream-snapshot-smart` makes Git request v2 from git-zero, checks version and ls-refs symref traces, and compares against an independently Git-created snapshot. |
| t5551:648–673 | SHA-256 server and empty-remote format discovery | Native `references` compares Git clients against git-zero and Git's smart/static servers in SHA-1 and SHA-256, including empty/custom unborn remotes, independent refs, tags, merge histories, clone/fetch and exact selected pack closures. Core `sha256` checks mismatched formats and resource boundaries. Complete git-zero client clone/materialization remains deferred. |

Additional `upstream-history-{smart,dumb}` and `upstream-packed-smart` cases use
merge parents, twelve changing historical blobs, nested paths, binary/empty
blobs, executables, symlinks, branch aliases, and lightweight/annotated tags.
Aggressive packing must demonstrably contain deltas: `git verify-pack` is checked
before testing either client. Smart git-zero downloads are imported with
`git index-pack --strict`, followed by `fsck --strict` and exact comparisons of
every reachable object's ID, kind and raw body. Returning a raw delta pack is
covered. The demo CLI still returns a raw pack; the core now also offers bounded
entry/delta decoding, checked against Git's IDs, index CRCs and bodies in
`tests/pack_reader.rs`. Local object-read benchmarks exercise those core APIs;
complete client fetch/materialization remains planned.

The native Cargo `head_states` suite additionally compares git-zero's server
with Git's HTTP backend/static bare oracle for empty/unborn and detached HEAD
with/without branch aliases. These are server discovery/clone/fetch comparisons
using real Git, distinct from the `upstream-empty` and `upstream-detached` client
regressions above.

The native `references` suite in `crates/git-zero-rustix/tests/references.rs`
uses `server_references.py` to compare independent branch tips, symbolic chains
(including HEAD through a lightweight commit tag),
merge histories, lightweight/annotated/nested tags and tree/blob tag targets.
Each case runs smart and dumb discovery, full and single-branch clones, fetches,
object-body comparisons and strict fsck against Git's oracle. Matched raw v0
upload requests compare exact selected closures, including duplicate and multiple
wants and peeled IDs. Unadvertised IDs are rejected even when reachable. Git's
smart client requests `include-tag` even with `--no-tags`; the Git reference clone
may physically receive extra tags peeling to its selected closure. Only that
oracle case permits those extra objects; matched no-capability requests and
git-zero's single-branch clones require the exact object set. V0 ACK/sideband
comparisons also check common-history exclusion. `server_protocol.py` compares
v1/v2 discovery, ls-refs, clone, incremental fetch, ACK rounds and exact selected
packs in SHA-1/SHA-256, including v2 nested include-tag behavior. `server_shallow.py`
adds depth-limited clones, absolute/relative deepening/unshallow and blob-filtered
no-checkout clones, comparing shallow files and delivered objects in both formats
and all three smart protocols. Time/exclusion deepening, promised-object
retrieval and v0 include-tag remain planned. The [server acceptance matrix](server-acceptance.md)
separates this server evidence from the client and Git-only baselines.

`upstream-snapshot-{smart,dumb}` constructs blob/tree/commit bytes using Git's
own commands, with the demo's exact content and identity. Both servers must
advertise identical refs and produce identical reachable objects and checkouts.
This provides an independent encoding oracle instead of deriving expectations
from git-zero's own response.

## Planned features

| Upstream cases | Feature and current boundary |
| --- | --- |
| t5550:205–225, 246–269 | Complete dumb traversal, manual object-graph fetching and explicit pack downloads. git-zero currently downloads only a loose HEAD. |
| t5550:294–339, 523–531 | Dumb pack indexes, including v1 indexes and reuse of downloaded indexes. Corruption has a Git reference baseline; git-zero does not import indexes yet. |
| t5550:426–449, 498–505; t5551:171–185 | HTTP redirects, redirect policy and base URL updates belong to richer host transports. |
| t5550:451–496, 507–521 | Dumb HTTP alternate object stores and bounded traversal across them. |
| t5551:20–22 | HTTP/2 belongs to a richer host transport. The current native transport is HTTP/1.0. |
| t5551:293–308 | Protocol disabling and discovery content-type validation. Server routes support both modes; the native client does not yet validate media types. |
| t5551:399–496 | Large ref/request budgets, chunked framing and reachable/arbitrary-object wants. Bounded selection accepts advertised HEAD/ref/peeled IDs; merely reachable IDs are rejected. Chunked framing and configurable broader want policies remain planned. |
| t5551:515–546 | Client ref updates and selecting wanted refs/IDs. The primitive smart fetch accepts an explicit ID; the demo fetch selects HEAD and does not maintain local refs. |
| t5551:593–596 | Buffered hosts can write terminal server ERR packets; client-side remote error decoding and exact upstream diagnostics remain planned. |
| t5551:675–686 | DNS and caller-provided hostname resolution. Native URLs currently require numeric loopback IPv4. |
| t5551:758–765 | Consistent trailing-slash URL normalization across host transports. |
| t5551:767–789 | Annotated-tag selection/following with shallow negotiation. Discovery and explicitly wanted tag closures are covered by native `references`; include-tag and shallow negotiation remain planned. |

## Outside the initial core scope

| Upstream cases | Responsibility |
| --- | --- |
| t5550:28–39 | Git CLI diagnostics outside any repository. git-zero exposes library errors and its own demo CLI. |
| t5550:79–83, 99–195; t5551:187–291, 334–397, 548–579, 598–637 | Apache authentication, credential helpers, cookies, redaction and submodule credential forwarding. Hosts own authentication; Git's CLI/configuration policy is not implemented by this core. |
| t5550:345–410; t5551:498–513, 581–591 | Git-specific diagnostics, translated errors, locale headers, arbitrary configured headers and curl trace controls. The host supplies HTTP policy and diagnostics. |
| t5550:412–424 | Git remote-helper diagnostics for malformed schemes. Native supported URL forms are documented separately. |
| t5551:310–332, 361–397 | Git filesystem namespaces and server hide-ref configuration. Synthetic repositories expose explicitly configured refs rather than reading Git server configuration. |
| t5551:688–756 | Git's username/password URL warnings and push behavior. Credentials belong to hosts; receive-pack is outside the initial read-only scope. |

## Parallel execution

All 24 cases can run simultaneously; cases share no fixture repository, port,
Git configuration, traversal state, cache, server or artifact filenames.
Dependencies such as creating a commit before fetching it stay inside one case.
The runner defaults to the available CPU count, accepts an explicit worker
budget, and validates names before starting any work.

```sh
python3 tests/interop.py --list-cases
python3 tests/interop.py --jobs 24
python3 tests/interop.py --shard 0/2 --artifacts /tmp/git-zero-traces &
python3 tests/interop.py --shard 1/2 --artifacts /tmp/git-zero-traces &
wait
python3 tests/interop.py --git /opt/git-2.53/bin/git --case upstream-packed-smart
```

Shards select disjoint positions from the same stable case list. Use the same
`--case` selection for every shard. Each invocation uses unique directories even
when artifact roots coincide. Without `--artifacts`, temporary directories are
removed. With it, fixture repositories, downloads, command/status logs, HTTP
request logs, Git packet/curl traces and server errors remain available. Curl
traces omit binary bodies; downloaded response files preserve the actual bytes.

CI runs independent Linux/macOS × system/pinned Git × two-shard jobs, with no
cross-job fixture or port sharing and no fail-fast cancellation. Failed jobs
upload their diagnostic directories. Local Cargo invokes the same full runner;
`GIT_ZERO_TEST_JOBS`, `GIT_ZERO_TEST_GIT`, and `GIT_ZERO_TEST_ARTIFACTS` configure
it without changing test code.

Relative deepening pins the wanted-history policy from
[Git 2.54 `shallow.c`](https://github.com/git/git/blob/v2.54.0/shallow.c):
compute the nearest client boundary from requested tips, then extend that depth.
[Git 2.53 `upload-pack.c`](https://github.com/git/git/blob/v2.53.0/upload-pack.c)
instead extends every reachable client boundary. The versioned multi-boundary
oracle retains this deliberate difference when fetching only one branch, verifies
each exact expected shallow file and compares all imported objects/checkouts.
Fetching all branches requires exact shallow-file equality for both algorithms.
