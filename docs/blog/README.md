# The guided tour

Eight posts, in order. The first three walk the layers from the bottom up; the
next four cover the API a caller actually writes against, and the last is for
anyone implementing a backend.

1. [How DocLite stores and finds
   things](01-how-doclite-stores-and-finds-things.md) is the storage layer:
   objects, index rows, order-preserving keys, and an operation as a value
   rather than a closure.
2. [The struct is the index](02-the-struct-is-the-index.md) is the derive:
   `#[derive(Document)]` turning a struct into its own indexes, query builder
   and revision history.
3. [Nothing is locked](03-nothing-is-locked.md) is what happens when two writers
   meet, and why a read that cannot branch is a read that can be written down.
4. [Several records at once](04-several-records-at-once.md) is the transaction:
   one unit of work, no `begin`, and a refusal that names the record that caused
   it.
5. [What the type settles](05-what-the-type-settles.md) is what is decided at
   compile time rather than by the store: `#[immutable]`, `#[storage(json)]`
   and `Ref<T>`.
6. [Where it is kept](06-where-it-is-kept.md) is the choice of backend, and what
   `Durability` is asking you.
7. [Being told what changed](07-being-told-what-changed.md) is the
   subscription system: `#[owner]`, catching up from a checkpoint, watching for
   what has not happened yet, and why a dropped notice costs a scan rather than
   an event.
8. [Two ways to be an engine](08-two-ways-to-be-an-engine.md) is how to write
   the next backend, and how the conformance suite tells you it is wrong.

These are living documents, like the ADRs. A post is rewritten in place when the
code it describes changes rather than corrected with a note underneath.

`crates/doclite-core/tests/docs_drift.rs` checks that every identifier these
posts name in backticks still exists in the source, that every link they make
resolves, that the items a code sample imports are still importable, and that
every attribute the derive accepts is described in at least one of them. The
same walk covers the rest of `docs/`, the README and `CONTEXT.md`. It runs with
the rest of the suite, so a post cannot quietly go stale.

What none of it can check is a claim about behaviour, which is worth tracing to
the test file the post names. A sentence counting how many of something there
are is the one that rots first, which is why these avoid counting.
