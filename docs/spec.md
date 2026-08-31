# The Same Design as a Composable API

A reframing of *A Tiny Transactional Storage VM* with the instruction set removed and a typed Rust API in its place.

---

## 1. The question underneath the question

"VM or API" sounds like a question about surface syntax. It isn't. The real question is:

```text
where does the control flow live?
```

There are only two answers, and they have very different consequences.

**Control flow in Rust.** The transaction body is a closure. `if`, `for`, `?` all work normally. The backend receives a closure it cannot inspect: it doesn't know whether the body writes, so it can't choose `BEGIN IMMEDIATE` over `BEGIN DEFERRED`; it can't re-run the body after a conflict, because the closure may have sent an email; it can't bound the work. This is the conventional `begin() / get() / insert() / commit()` API the design document already rejected, and it deserves rejecting for those reasons.

**Control flow in the data structure.** The operation is a *value*. The backend can inspect it, plan for it, and re-run it. This is what the design document arrived at, and once control flow is data, you have a VM whether or not you call it one.

So the choice is not "VM or composable API." It is: **does the composable API sit on top of a reified operation, or on top of a closure?** Keep the reified operation. Delete the assembly language. That is the entire reframing, and it turns out to remove a great deal of the project.

---

## 2. Why so little is lost

Look at what the VM's control flow was actually used for. Not what it *could* express, what the five worked programs needed:

| Program | Control flow used for |
|---|---|
| 5.1 insert if unique | one precondition check |
| 5.2 replace revision | one precondition check |
| 5.3 scan appointments | iterate a range |
| 5.4 delete prefix | none |
| 5.5 two-index uniqueness | two precondition checks |

That's two patterns: **check a precondition** and **iterate a range**. Nothing else.

And this is not a small sample being unrepresentative. It follows from the design's own constraints. There is no arithmetic, so nothing can accumulate. There is no deserialization, so a program can never branch on what an object *contains*: the VM can compare counts, keys, and ids, and that is all. A general branching machine whose only observable values are index counts and byte strings can express precondition checks and loops over ranges. It cannot express much else, because there is nothing else to be data-dependent about.

The generality was mostly unused. Two combinators reclaim nearly all of it:

```text
require_*   replaces branch + ABORT
scan        replaces INDEX_ITER + ITER_NEXT + JUMP loop
```

Once `scan` absorbs iteration, there are no backward jumps. No backward jumps means no non-termination, which means fuel stops being a safety mechanism and becomes a simple result-size cap.

---

## 3. The API

### 3.1 Typed index handles

Arity is a const parameter, so key arity is a compile error rather than a verification error:

```rust
pub struct Index<const N: usize> {
    name: &'static str,
}

static USERS_BY_EMAIL: Index<1> = Index::new("users_by_email");
static APPTS_BY_DOCTOR_DATE_TIME: Index<3> = Index::new("appts_by_doctor_date_time");
```

Keys carry their arity too:

```rust
key![email]                  // Key<1>
key![doctor_id, date]        // Key<2>, a partial key over a 3-arity index
```

### 3.2 Ranges

```rust
exact(key![email])                        // Range<1, 1>  full key
prefix(key![doctor_id, date])             // Range<3, 2>  partial, over an arity-3 index
between(key![lo], key![hi], Bound::Incl, Bound::Excl)
```

The two const parameters are the index arity and the key arity, so `exact` on a partial key and `prefix` with too many components both fail to compile.

### 3.3 Write operations

```rust
Operation::new()
    // preconditions, each carrying the abort code used when it fails
    .require_absent(&idx, range, code)
    .require_count(&idx, range, n, code)
    .require_object(id, code)

    // mutations, applied in the order written
    .put_object(id, bytes)
    .put_index(&idx, key, id)
    .delete_index(&idx, range)

    // result
    .returning(value)
```

### 3.4 Read operations

```rust
Query::count(&idx, range)                       -> u64
Query::object(id)                               -> Option<Bytes>
Query::scan(&idx, range)                        -> Vec<Row>
Query::scan(&idx, range).limit(n)
Query::scan(&idx, range).limit(n).with_objects::<T>()  -> Vec<(Row, T)>
Query::zip(a, b)                                -> (A, B)   one transaction
```

`zip` is what replaces "several reads in one program." It composes, so three or four consistent reads in a single snapshot stay a single expression.

### 3.5 Execution

```rust
let outcome = db.run(&operation)?;
```

`run` takes the operation by reference, inspects it once, opens the right kind of transaction, and retries the whole thing on conflict, because the operation is a value with no side effects, so re-running it is always safe.

```rust
pub enum Outcome<T> {
    Committed(T),
    Aborted(AbortCode),
}
```

`Result<Outcome<T>, StorageError>`: a duplicate email is `Ok(Aborted(...))`, a disk failure is `Err`.

### 3.6 Where BEGIN, COMMIT, ROLLBACK and ABORT went

They became the type. An `Operation` *is* a transaction's worth of work, and `run` is the boundary:

```text
BEGIN      the start of run()
COMMIT     run() reaching the end of the step list without aborting
ROLLBACK   automatic, any abort or error unwinds the whole operation
ABORT      a require_* step failing, surfaced as Outcome::Aborted(code)
```

Nothing was dropped. What changed is that an unbalanced transaction is no longer constructible. Three of the verifier's checks (exactly one `BEGIN`, `COMMIT` requires an active transaction, `RETURN` requires no active transaction) were guarding a state machine that the caller could get wrong by writing the wrong instruction. There is now no instruction to write.

**Steps are one ordered list.** Preconditions and mutations interleave in written order, and a precondition placed after a mutation observes that mutation. Read-your-own-writes is preserved exactly as the design document specifies:

```rust
Operation::new()
    .put_index(&USERS_BY_EMAIL, key![email], id)
    .require_count(&USERS_BY_EMAIL, exact(key![email]), 1, Abort::INVARIANT)
```

The API sketch in 3.3 lists preconditions first only because that is the common shape, not because the builder enforces it.

**Composition is what replaces the delimiters.** Two operations built by different modules merge into one transaction:

```rust
let op = create_patient(&patient).then(open_account(&patient));
```

`then` concatenates step lists. The result is still one `Operation`, so it is still one transaction: the boundary moved from a pair of instructions to a value you can pass around and combine. On the read side, `Query::zip` does the same job: several reads, one snapshot.

**There is deliberately no `db.transaction(|tx| ...)`.** It is the first thing that will feel missing, and adding it would undo the whole design: a closure body cannot be inspected for writes, so the backend loses its lock-mode choice, and it cannot be safely re-run, so automatic retry goes with it. If a transaction is needed, build an `Operation`.

**Two transactions means two calls to `run`.** That is consistent with decision 2.5 (one transaction per operation) and with the VM version, which had the same restriction.

**Explicit `ABORT` anywhere is not missing, either.** Every abort is one of two kinds. If the condition depends on storage state, it is a `require_*` step. If it does not, it can be evaluated in ordinary Rust *before* the operation is built, because nothing about it will change inside the transaction:

```rust
if !email.is_valid() {
    return Err(AppError::InvalidEmail);   // never reaches the storage layer
}
```

The VM had to express both kinds as `ABORT` because it had no "before." The API does.

---

## 4. The five programs

### 4.1 Insert a user if the email is unique

```rust
let op = Operation::new()
    .require_absent(&USERS_BY_EMAIL, exact(key![email]), Abort::DUPLICATE_EMAIL)
    .put_object(id, blob)
    .put_index(&USERS_BY_EMAIL, key![email], id)
    .returning(id);

match db.run(&op)? {
    Outcome::Committed(id) => ...,
    Outcome::Aborted(Abort::DUPLICATE_EMAIL) => ...,
    Outcome::Aborted(_) => unreachable!(),
}
```

Twenty lines of assembly, four lines of Rust, identical semantics and identical execution plan.

### 4.2 Replace a logical object

```rust
let op = Operation::new()
    .require_absent(&USERS_BY_EMAIL, exact(key![new_email]), Abort::DUPLICATE_EMAIL)
    .put_object(new_id, new_blob)
    .delete_index(&USERS_BY_EMAIL, exact(key![old_email]))
    .put_index(&USERS_BY_EMAIL, key![new_email], new_id)
    .returning(new_id);
```

Mutations apply in written order, so the delete-then-insert sequence is explicit, and the whole swap is still one transaction.

### 4.3 Scan a range

```rust
let q = Query::scan(&APPTS_BY_DOCTOR_DATE_TIME, prefix(key![doctor_id, date]))
    .limit(500)
    .with_objects::<Bytes>();

for (row, body) in db.run(&q)? {
    let time = row.key.component(2);   // straight out of the key, no object read
    ...
}
```

The dangling-entry case from the VM version, an index row whose object is missing, becomes a decision on the builder rather than a jump target:

```rust
.with_objects()          // dangling entries abort: Abort::DANGLING_INDEX_ENTRY
.with_objects_lenient()  // dangling entries yield None
```

### 4.4 Delete every entry under a prefix

```rust
let op = Operation::new()
    .delete_index(&USERS_BY_COUNTRY, prefix(key![country]));
```

### 4.5 Uniqueness across two indexes

```rust
let op = Operation::new()
    .require_absent(&USERS_BY_EMAIL, exact(key![email]), Abort::DUPLICATE_EMAIL)
    .require_absent(&USERS_BY_CI,    exact(key![ci]),    Abort::DUPLICATE_CI)
    .put_object(id, blob)
    .put_index(&USERS_BY_EMAIL, key![email], id)
    .put_index(&USERS_BY_CI,    key![ci],    id)
    .returning(id);
```

Preconditions compose by chaining. Adding a third unique field is one line, where the assembly version needed a new label, a new comparison, and a new branch.

---

## 5. The one case that genuinely needs more

Reindexing: read every entry under a range and write a derived entry for each. The row values are only known at execution time, so this is real data dependence, the only kind the design admits.

```rust
let op = Operation::new()
    .for_each(
        Query::scan(&PATIENTS_BY_CLINIC, prefix(key![clinic_id])),
        |row| Operation::new()
            .put_index(&PATIENTS_BY_CLINIC_V2, key![row.key(0), row.doc()], row.doc()),
    );
```

The closure is invoked **once, at build time**, with a symbolic `row`. What comes back is an operation template that the engine instantiates per row. The type system enforces the restriction that makes this sound: `RowRef` yields symbolic values with no `PartialEq`, so a body cannot branch on the row, it can only construct.

This is the one sharp edge in the API, and it should be documented as such. It is also the only combinator that reintroduces unbounded work, so it takes an explicit `.limit()`.

Cascade delete has the same shape. Everything else in the design does not need it.

---

## 6. What you keep, and what you give up

Kept, in full:

- **Whole-operation-up-front.** `run` inspects the operation before executing: writes present means `BEGIN IMMEDIATE`; no writes means `BEGIN DEFERRED`. Index handles resolve in one batched catalog lookup.
- **Automatic retry.** Operations are values with no side effects, so conflict handling stays inside `run`.
- **No round trips.** Preconditions and mutations still execute in one transactional context with no return to application code.
- **Backend portability.** Unchanged. The internal representation is still an ordered list of steps against the same backend trait, and the testkit is still the definition of a correct backend.
- **Serializability of operations.** Keep the internal representation encodable. It is what you would need if operations ever cross a process boundary.

Given up:

- **Arbitrary control flow.** Nested conditionals, loops with early exit on a computed condition, branching on an object body. The last was never possible anyway; the first two have no known use in this design.
- **Programs as a portable artifact for untrusted callers.** Operations are constructed by Rust code, so an untrusted party cannot submit one. This is the trigger condition for bringing the VM back, see below.

Gained:

- **Verification moves into `rustc`.** Arity mismatches, type errors, and transaction-state errors become compile errors. Registers-initialized-before-use, jump-target validity, and the abstract-interpretation lattice all cease to exist as concepts.
- **Fuel simplifies to a result cap.** No backward jumps means no non-termination, so limits exist to bound memory rather than to guarantee halting.
- **Prepared programs stop being necessary.** Building an operation allocates a small `Vec` of enum variants, tens of nanoseconds against a transaction that will fsync. Construct one per request and stop thinking about it.

---

## 7. When to bring the VM back

One condition, and it is worth writing into the design document so the decision is not re-litigated on vibes:

> An untrusted or out-of-process party needs to submit a storage operation.

An HTTP service accepting operations from clients, a plugin system, a replication protocol shipping operations rather than results. At that point you need an encodable, verifiable, fuel-limited representation, and the composable API becomes the compiler that targets it: exactly the "higher-level Rust API" the design document already anticipated, with the layers built in the opposite order from the plan.

Nothing else triggers it. In particular, "the operations got complicated" does not: an operation that needs real branching almost certainly needs to branch on object *contents*, and no version of this design can do that.

Design for it cheaply now: keep the internal step list a plain serializable enum, keep it separate from the builder, and don't let backend-specific values leak into it. That is a few hours of discipline, and it keeps the VM available without building it.

---

## 8. Plan deltas

| Milestone | Change |
|---|---|
| **M0** semantics | Unchanged in substance. Isolation, iterator rules, key encoding, and `ObjectId` policy are all API-shape-independent. The opcode table becomes a step-type table. |
| **M1** encoding + memory backend | Unchanged. |
| **M2** VM core to **operation engine** | Roughly halves. No register file, no dispatch loop, no jump resolution, no label fixups. An ordered walk over steps, plus `for_each` instantiation. |
| **M3** verifier | Mostly disappears. What remains is a structural check on the built operation (index handles resolve, `for_each` carries a limit) which is a few hundred lines, not a dataflow analysis. **Saves 1 to 2 weeks and the highest-variance work in the project.** |
| **M4** SQLite | Unchanged. `docs/sqlite.md` is the design. |
| **M5** concurrency | Unchanged, and still the riskiest milestone. This reframing does nothing for the hard part. |
| **M6** prepared programs | Evaporates. `require_absent` is now the short-circuit fast path directly. |
| **M7** typed layer | Partly absorbed. `Collection<T>` composes operations rather than compiling to programs. |

Net: about three weeks shorter, and the removed work is the part with the least certain estimate.

**The one thing to be honest about:** none of this touches M5. Serializable isolation, predicate assertions holding between observation and commit, and cross-backend agreement under concurrency are exactly as hard as before. The reframing removes the fun part of the project and leaves the difficult part untouched, which is a good trade, and worth naming, because the VM was the more enjoyable thing to build.
