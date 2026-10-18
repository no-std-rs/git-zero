# R16 small-receive candidate

Source: `603b224eda3719c7525db00ac6a112a71368d7b3`; test-only extension: `a624a789b98570fabd324ab0a16b803daa4641be`.

Eight same-source production pairs save one Worker and one DO callback per
activated small push, with identical Full object I/O and writes. This is
correctness/topology evidence; net cost and speed remain unmeasured.

The ledger retains all actual fault/fallback/warm/source scopes and failed
attempts. Unknown/incomplete receipts stay unknown/incomplete; full bill is NULL.
Repeated large fixture/pack payloads remain local with complete SHA/size inventories.

- [Implementation and artifact ledger](implementation.json)
- [Compact raw evidence](raw-evidence.tar.gz)
- [Publication validation](publication-validation.json)
- [Protocol and limits](../../../../hosting-small-receive.md)

Independent [source review](independent-source-review.md) and [ledger continuity review](independent-ledger-review.md) passed. Fresh screens and final five-pair validation remain.

Injected after-PUT failure receipts contain zero acknowledged written bytes; the physical write is proven by the wrapper and recovery inventory. Those incomplete fault traces are not priced as successful benchmarks.
