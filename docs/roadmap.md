# Roadmap

What 0.3.0 is for, and what is absent on purpose. Every item here is something
the code or the documents already point at, so a claim in this file can be
traced rather than taken.

Like the ADRs, this is a living document. An item is deleted when it is done
rather than ticked, because a roadmap of finished work is a changelog and there
is already one of those.

## 0.3.0

None of this blocks a release. Each item is a thing the design already names as
deferred, or a gap that only shows up once somebody else is using the crate.

### A reader pool for the SQLite engine

Reads serialise behind writes, because the engine owns one connection thread and
that thread is the only thing that touches the connection. `docs/sqlite.md` and
ADR 0013 both name the fix and both decline to build it first: WAL allows
concurrent readers on separate connections, so a pool of readers alongside the
single writer is the change.

It was deliberately not the first version, because one thread is the version
whose correctness is obvious. It becomes worth the complexity the first time a
read-heavy caller measures the queue.

### Something to measure with

There are no benchmarks. That is uncomfortable for a design that justifies parts
of itself by the work they save: `Engine::plan` exists so a count is counted
where the rows are, a sweep exists so a template is written in the backend's own
language, and the reader pool above is a performance change with nothing to
prove it helped.

Benchmarks would also give the hint contract a second kind of test. The
conformance suite already proves a hint never changes an answer, by running
every script bare and probed and demanding identical transcripts. Nothing yet
shows a hint ever saved anything.

### A second `Store` implementation

There are two ways to be an engine, and the narrow one has exactly one user: the
in-process map. A seam with one implementation is a seam that happens to fit its
only user, and ADR 0011 names redb, sled and RocksDB as the shape it was drawn
for.

Writing one is the cheapest way to find out whether `Store` and `Region` are the
right size, and the conformance suite means the new backend arrives with its
correctness argument already written.

`Store::define` answers a `Defined` and `Store::rebuilt` records one as
filled, so a store that keeps its rows has somewhere to say which indexes it
has no complete set of rows for, and somewhere to say when it does. The
in-process map answers `Defined::ready` truthfully because nothing it holds
outlives the process; a persistent one is the first that will have to mean
both.

### Decide `#[non_exhaustive]` before 1.0

No public enum carries it, including the error enums. Before 1.0 that is
covered, because a minor bump is where a breaking change goes and the changelog
says so. After 1.0 adding a variant becomes breaking.

The argument for leaving it off is not laziness: `#[non_exhaustive]` forces
every downstream `match` into a catch-all arm, which is exactly the pattern
this project's own hygiene rules ban. Both positions are defensible, which is
why this should be a decision with a date rather than an omission nobody
revisits.

## History that only exists here

Three branches point at the history that existed before it was collapsed into
one commit: `master` (a5e016a), `sqlite-engine` (600fc7e) and
`feature/index-versioning` (2d51e5d). None shares a merge base with `main`, so
`git diff` against any of them fails outright.

They are kept on the machine the work happens on and are not pushed, so the
published repository is the single line `main` describes. That makes them a
backup of exactly one copy, which is the cost of keeping the published history
clean.

## Known sharp edges

Small, real, and each one already written down where it bites.

- **The byte-field refusal is syntactic.** `#[storage(json)]` is refused on a
  record with a visible `Vec<u8>`, `[u8; N]` or `Option<Vec<u8>>`, by reading
  the field's written type. One behind a type alias or inside a newtype goes
  through and encodes as an array of numbers. Catching it properly needs type
  information a derive does not have.
- **A JSON envelope writes ids as numbers.** They sit above the range a reader
  whose integers are floats can hold exactly, so a browser parsing one loses
  precision. SQLite's own JSON functions read them exactly, which is the case
  the format was chosen for.
- **The generated index cannot be rebuilt.** A rebuild pages
  `<record>_latest` to find the live revisions, so that index is the one thing
  it cannot derive. A record type new to the store has every index of it
  reported and nothing to walk, which is ordinary; the generated index missing
  rows while an index derived from it is not is refused as
  `DocumentError::MissingLatest`. Nothing in the crate produces that state.
- **Every index of a type dropped at once reads as a new type.** Both present an
  empty generated index on a store holding objects, and separating them needs
  the objects of that type enumerated, which `Engine::read` cannot be asked for.
  The rebuild files nothing and records them complete, so those records stay
  unfindable and no later open retries. Reaching it means something outside this
  crate edited the database, and it is the one failure here that is quiet rather
  than loud. ADR 0015.
- **A list of owners is recognised syntactically.** `#[owner]` reads the
  field's written type, so `Vec<T>` is several owners and anything else is one.
  A `type Owners = Vec<Ref<User>>` alias or a newtype around one is read as a
  single owner and fails to compile as a column. The same limit as the
  `#[storage(json)]` byte-field refusal above, and the same reason: a derive
  has no type information.
- **A feed's order is mint order, not commit order.** An id is minted while an
  operation is built, so a slow transaction can commit after a change with a
  higher revision is already visible, and a reader resuming from the highest
  one it saw can miss it. The fix is the reader's: lag the checkpoint it keeps
  by its longest build-to-commit time, and let the at-least-once delivery
  absorb the overlap. Closing it properly needs a sequence the store assigns,
  which ADR 0007 rules out. The live path has no such gap.
- **A feed added to an existing store is filled from the live revision of each
  document.** A rebuild pages `<record>_latest`, which names live revisions
  only, so the first catch-up reads as a snapshot rather than as a history even
  though the older revisions are still there. Walking `prev` to fill the whole
  history is possible and is not done. ADR 0016.
- **A reassignment is invisible as a departure.** Moving a record from one
  owner to another ends one feed and starts another, and the old owner is not
  told it left. A subscriber mirroring that owner keeps a stale copy.
- **Nothing frees a feed.** It is the one index whose size grows with edits
  rather than with records. Pruning is a range delete an operation can already
  express, but the key order decides what is cheap: by owner is one range, by
  time is one range per owner.
- **One contention test has failed once.** `one_writer_takes_the_key` failed
  during a full parallel run and did not reproduce across eleven later runs. The
  cause is unverified. The plausible candidate is the retry loop exhausting its
  attempts under load, which would surface as the arm of that test which
  expects a write to be either filed or refused.

### More of what a feed could be

`#[owner]` takes one field, and a record has one feed. Each of these is named
by the code or the ADR as deferred rather than missing:

An owner made of several fields. `#[owner(clinic, patient)]` is refused today,
because reading it as the first component alone would file every change under
the wrong key.

A watch that outlives its scope. `Changes` borrows the connection, so a watcher
lives where the store was opened rather than being handed to a task that
outlives it.

Delivery between processes. The bus is one connection's, so a second process
writing the same database is not seen. The catch-up scan on a timer is the
answer that already works; a notifier over SQLite's update hook would be an
optimisation of it.

## Not on the roadmap

Absent on purpose, so that finding them missing is not mistaken for finding them
unfinished. Each is argued where it is named.

- **A closure transaction.** There is no `db.transaction(|tx| ..)`, and adding
  one would undo the design: a closure body cannot be inspected for writes, so
  the backend loses its lock-mode choice, and it cannot be safely re-run, so
  automatic retry goes with it. ADR 0003.
- **An explicit abort step.** A condition that depends on stored state is a
  precondition; one that does not can be evaluated in ordinary Rust before the
  operation is built, because nothing about it changes inside the transaction.
  `docs/spec.md` section 3.6.
- **Deleting an object.** Objects are appended and never rewritten, and nothing
  frees one. A layer that wants retirement models it as an ordinary indexed
  field, because a tombstone is erasure wearing an append's clothes. ADR 0001.
- **A rule declared on an index.** An index is a set of rows and carries no
  constraint. Whether at most one object may hold a key is asserted by an
  operation, which is what lets one index answer for two rules. ADR 0001.
