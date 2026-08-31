# Architecture decision records

These are living documents. An ADR is rewritten in place when the code it
describes changes; there is no supersession chain and no record here of how a
decision was reached. A claim that stopped being true is removed or restated,
never patched with a follow-up note. Every ADR's status is Accepted, because an
ADR that is not is just a file nobody deleted.

`crates/doclite-core/tests/docs_drift.rs` checks that every identifier an ADR
names in backticks still exists in the source, that every link it makes
resolves, and that the items a code sample imports are still importable. It runs
with the rest of the suite, so an ADR cannot quietly go stale. The same walk
covers the rest of `docs/`, including the spec and the guided tour, as well as
the README and `CONTEXT.md`.

What it cannot check is a claim, so a sentence stating how many of something
there are will pass while being false. These avoid counting for that reason.
