# Deferred catalog update proposal

[Independent design](design.md) and [review](independent-review.md) describe a
bounded next candidate after packed catalog pages. Folding a confirmed
retirement's page update into the next upload offers $0.028 gross per 1,000
four-profile replays, before actual recovery/proof/cursor overhead. Epoch
observations are retained in `epochs.json.gz`.

Implementation is being prepared in isolation. There is no measured saving,
acceptance or stopping evidence. The design preserves actual deletes and durable
publication ordering; proofs require exact receipts, durable replay protection
and an exceptional pre-upload barrier for resurrected keys. Observers cannot
repair or certify production state. Fresh measurements must charge all fallbacks.
