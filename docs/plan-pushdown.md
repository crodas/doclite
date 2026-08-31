# Letting an Engine Plan

A backend with a query planner should be allowed to use it. This document
specifies the seam that lets one, and the rule that keeps it safe.

It is written for the SQLite engine (`docs/sqlite.md`) because that is the first
backend that can take advantage of it, but nothing here is about SQLite.

## What an engine is not told today

`Engine::read` takes a `Reads`: a flat list of scans and a flat list of object
ids. That is not the shape the caller asked for. A `Query` builds a `ReadPlan`,
and `flatten` in `crates/doclite-core/src/executor/answer.rs` takes it apart
before any engine sees it.

Two things are lost on the way.

**A count is issued as a scan.** `ReadPlan::Count` becomes an `IndexRead` with
no limit, the engine returns every matching row, and the executor counts them:

```rust
Slot::Count { range, .. } => Ok(ReadValue::Count(range.select(rows).len() as u64)),
```

Counting a million rows moves a million rows so that `len` can be called on
them. A backend that can answer `SELECT COUNT(*)` is never asked to.

**A zip becomes two unrelated scans.** `ReadPlan::Zip(a, b)` exists to say that
two reads came from one view of the store and can therefore be trusted to agree.
`flatten` pushes both halves onto the same list and `rebuild` pairs the answers
back up afterwards. The engine is told to answer both from one view, which is
the part that matters, but it is not told they belong together, so it cannot
make them one statement.

On the write path the loss is larger. `Step::ForEach` is expanded row by row in
`crates/doclite-core/src/executor/walk.rs`: the executor scans, then runs the
template against each row, and the engine receives a `Batch` of concrete rows
with no idea they were derived from a scan it could have done itself. A reindex
that a backend would write as one `INSERT INTO ... SELECT` arrives as N rows
that crossed the process boundary twice.

## The decision

An engine may be handed the plan, and may decline it.

```rust
/// What this plan answers, if the engine planned it.
///
/// `None` means it did not, and the executor reads the ordinary way. An
/// engine that never implements this is unaffected.
async fn plan(&self, plan: &ReadPlan) -> Result<Option<ReadValue>, StorageError> {
    let _ = plan;
    Ok(None)
}
```

`ReadPlan` and `ReadValue` are already public and already mirror each other, so
the seam introduces no type. `ReadValue::Count`, `Rows`, `RowsWithObjects`,
`Object` and `Pair` are exactly the shapes a plan node produces, which is what
makes the signature honest: the engine is asked the question the caller actually
asked, and answers in the caller's shape.

The executor calls `plan` first. On `Some`, that is the answer. On `None`, it
flattens and reads as it does today. There is no third path and no partial
result: an engine that can plan half a `Zip` declines the whole thing.

## The rule that makes it safe

**The plan is a hint.** It joins `range` and `limit` in ADR 0004's table, and it
is a hint in the strong sense that table means: ignoring it costs time and never
correctness.

This is not an assertion, it is a test. `Probe` in
`crates/doclite-testkit/src/probe.rs` already strips `range` and `limit` before
forwarding. It gains one more strip: `plan` returns `None` unconditionally. The
`hints_are_hints` property then runs the same script against a bare engine and a
probed one and demands identical transcripts, so a planning engine that computes
a different answer from the flattened path fails a test that already exists.

That is the whole safety argument, and it is why this seam is worth adding
rather than merely defensible. The mechanism that proves a superset is harmless
is the same mechanism that proves a pushdown is harmless.

## Deciding a `for_each`

A `for_each` cannot ride the read seam, because it is not a read. It is worth
the extra design because it is where the win is largest: a reindex that a
backend would write as one `INSERT INTO ... SELECT` arrives as N rows that
crossed the process boundary twice.

An unexpanded one is a **sweep**: a scan, and a template instantiated once per
row, handed over whole.

### Why the first version of this was wrong

An earlier draft said a sweep could be offered on the same terms as a plan, and
that was false. The mistake is worth keeping, because the reasoning that makes
it false is the reasoning that makes the rule right.

`Step::ForEach` runs each template step through `Pass::step`, which stages into
the executor's `Delta`. A later `Step::Require` folds that overlay before
judging:

```rust
let visible = delta.rows_in(*index, &range, committed);
```

So a precondition placed after a `for_each`, on an index the template wrote,
sees those writes. Defer the `for_each` into `apply` and nothing is staged, so
the same precondition sees pre-batch state and can reach the opposite verdict.

That is a different answer, not merely different work, which is exactly what a
hint may never be. `hints_are_hints` did not catch it either, because its script
contained no `for_each` at all.

### The rule

**A `for_each` may be deferred only when it is the whole operation.**

One step, and all three hazards disappear together rather than being managed:

- Nothing earlier staged writes, so the sweep's scan sees the same pre-batch
state it sees today.
- Nothing later reads, so the missing overlay cannot be observed by anything.
- The scan and the writes derived from it happen in one transaction, so the
`Observed::Rows` assertion the executor would have recorded has nothing left to
protect. It is not optimised away; it stops being meaningful.

That third one is ADR 0005's argument applied one level down. An assertion
carries consistency across a gap, and here there is no gap.

The rule is narrow and it is not a compromise. The two worked programs the
sweep is for, `for_each_writes_a_derived_entry_per_row` and
`for_each_deletes_a_range_named_by_each_row` in
`crates/doclite-testkit/src/programs.rs`, are exactly this shape: a reindex and
a cascading delete, each `Operation::new().for_each(..)` and nothing else.

### How the sweep gets there

`Batch` carries it as a variant rather than a field, which makes the ordering
question inside `apply` unaskable: **a batch is either writes or a sweep.**

```rust
pub enum Batch<'a> {
    Writes { asserts, objects, delete, insert },
    Sweep(&'a Sweep),
}
```

It was a field once, and every half of the protocol had to remember not to mix
the two. `doclite-store` did not read the field at all, so a store that claimed
`takes_sweep` would have written nothing and answered `Applied::Committed`. The
match is what closes that: a store cannot build an `Admission` from a sweep, and
says so where it used to report success.

The variant also carries no assertions, because there are none. A sweep is only
ever a lone `for_each`, so nothing ran before it to record one.

The executor asks once, with the sweep in hand:

```rust
fn takes_sweep(&self, sweep: &Sweep) -> bool { let _ = sweep; false }
```

Taking the sweep rather than being a bare flag is what keeps the answer honest:
an engine inspects what it is offered and declines what it cannot write, rather
than promising in the abstract.

### What an engine declines

`check.rs` refuses a nested `for_each` and nothing else, so a template may hold
any other step. `Step::Require` and `Step::RequireObject` inside one are per-row
preconditions that abort the whole operation with a code, and a single statement
does not express "check this for every row, and if any fails, abort with code
100 and change nothing". `Step::PutObject` in a template names one fixed id, so
it can only mean something for a single row.

Everything else is a construction. ADR 0003 guarantees it: a template is built
once with a `RowRef` handing out `Cell`s that carry no `PartialEq`, so a
template can construct and cannot branch. A branchless per-row construction is
what `INSERT INTO ... SELECT` is.

One convenience falls out of `check.rs`. `bounded` requires a `for_each` scan to
be `ReadPlan::Scan { limit: Some(_) }`, so a sweep always carries a limit and
the generated statement always has a `LIMIT`. It also needs the matching `ORDER
BY`: the executor takes the first rows in index order, and a statement that took
a different set would be a different answer rather than a faster one.

### Why this one is safe

The same way the read seam is. `Probe` declines `takes_sweep` along with the
other hints, and `hints_are_hints` runs a `for_each` program bare and probed and
demands identical transcripts. An engine that sweeps and one that expands have
to agree, and now something says so.

### The overlay gap underneath all of this

A `for_each` reads committed state and not the operation's own overlay. In
`walk.rs`, `Step::Require` folds the delta with `delta.rows_in` before judging;
`Step::ForEach` calls `scan_engine` and iterates the raw result. A `for_each`
placed after a staged write on the index it scans does not see that write, and
that is true today, before any of this.

The only-step rule makes it unreachable rather than fixing it: with one step
there is nothing staged for the scan to have missed. Whether the general
behaviour is intended is still an open question, and it should be settled before
the rule is widened.

## Consequences

Every existing engine is unaffected. `plan` and `takes_sweep` have defaults, so
`MemoryEngine`, `StoreEngine`, `Probe`, and the test doubles `Balk` and
`Counted` compile and behave as before. A decorator that forwards nothing
declines, which turns the pushdown off for whatever it wraps without changing an
answer. That is the correct failure, but it is quiet, so a wrapper meant to be
transparent has to forward `plan` on purpose.

The executor keeps every line it has. Nothing is deleted, because the fallback
path is the current path. The pushdown is pure addition, which is what makes it
safe to add before a second engine exists to use it.

ADR 0004 gains a row in its hints table and keeps its story about the hint it
removed. That story is the precedent being followed here rather than broken: a
candidate list was specified, nothing ever produced one, and it was deleted. A
hint nothing sends is not a hint. This one is added because something sends it,
and it should be added at the same time as the engine that does.

An engine can now be wrong in a new way: it can plan a query and get a different
answer than the flat path would have. That failure is invisible to review and
visible to `hints_are_hints`, which is the trade this project has already made
twice.
