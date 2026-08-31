# ADR 0006: An Async Engine Trait

## Status
Accepted

## Context
The composable API reads as synchronous, and the executor does not itself need a
runtime. A durable engine does. A file-backed or networked store wants to yield
while it waits, and a sync trait forces every caller in a runtime to wrap each
call in a blocking pool.

## Decision
`Engine` is `#[async_trait]`, and so is `Runnable`. `Db::run` is awaited.

## Consequences
`doclite-core` depends on `async-trait` and on nothing else in the workspace. It
does not depend on a runtime, so which one a caller uses is not this crate's
business.

Every `Engine` method allocates a boxed future per call. Against a transaction
that will fsync this does not register, and the executor makes few calls per
operation: one per precondition, one per query, one to apply.

A synchronous caller drives a `Db` with a current-thread runtime, which is what
the `doclite-memory` doctest does.
