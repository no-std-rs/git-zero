# Standard-VM fetch diagnostic activation

The [independent review](independent-REVIEW.md) approves exact source `5d7f6a4`
and its tiny proof. The [activation decision](activation-decision.json) authorizes
one two-case serial diagnostic on standard VM disk with local emulated services.
Fresh capacity must pass before fixture/service startup; no automatic retry or
paid Cloudflare deployment is authorized.

Only workflow/docs are added on this isolated branch. Code is checked out and
built from pinned `5d7f6a4`; actual fresh release binaries and VM/source/phase
identities are retained. This is a new diagnostic cohort, not a performance
acceptance test. Historical cold environment remains NULL. The original
optimization branch and its running R20/R21 screen remain frozen and untouched.

Strict Clippy was not established for this diagnostic source: two unchanged
pre-existing lints were allowed by its retained author check. No speed, cause,
fix, full bill or final-five validation follows from activation.
