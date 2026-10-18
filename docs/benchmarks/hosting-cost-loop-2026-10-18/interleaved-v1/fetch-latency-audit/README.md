# Fetch latency investigation

The [read-only audit](AUDIT.md) separates the codec elapsed counter from view
preparation, callbacks and full response delivery. Warm zero-R2 tails exclude
object reads for those requests; the remaining cause is unresolved. CPU stays
unknown. Historical input identities and scope are in [scope.json](scope.json).

The audit identifies lost failed-curl output in a local test helper, which its
owner corrects separately. It proposes bounded Rust-only phase diagnostics; no
production fix or benchmark improvement follows. An isolated tiny real-Git
diagnostic proof is authorized, with review required before large replay.

Check exact publication bytes with `sha256sum --check SHA256SUMS`.

[Stage-one independent review](stage1-independent-review.md) approves only the
isolated opt-in hooks and tiny correctness proof at `a827df0`. It preserves
partial writes, actual failed HTTP bodies and unknown CPU values. The initial
runner has a cooperative budget; a separate hard supervisor needs review before
large replay. The [relocation map](stage1-immutable-relocation-map.json.gz) pins
local immutable source and ELF copies before later builds reuse old paths.
No production latency fix or benchmark improvement is established.

[Stage-two independent review](stage2-independent-review.md) approves the
separate Linux supervisor and tiny stall/cleanup evidence. Both large-case
plans remain unexecuted. Root authorized the bounded executor implementation
and a tiny same-path proof; large replay still requires frozen-source review
and a separate resource decision. The two [sealed plans](stage2-case-plans.json)
keep fixture reconstruction unverified/null.
