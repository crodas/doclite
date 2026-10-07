# ADR 0015: An Index Is Filled Before It Is Read

## Status
Accepted

## Context
`define` is handed the whole catalog on every open, and a declaration it has
never seen is answered by creating a table. Nothing was recorded about the shape
that table was built for and nothing about whether its rows were all there, so
two things went wrong.

An index new to a store that already held objects got an empty table. Every
query through it answered correctly-shaped nothing, and a caller could not tell
that apart from a store holding no matching rows. That is the worst kind of
failure this design can have: not a refusal, an answer.

An index whose columns had moved kept the table it already had, because
`CREATE TABLE IF NOT EXISTS` leaves one alone. `Catalog::declare` refuses a
disagreement it can see, but a `Catalog` is built per connection and so cannot
see across a restart; the declared layout and the real columns then drift apart
and the first write fails as an arity nobody wrote.

ADR 0001 already said what to do and did not do it: "an index can be rebuilt
from the objects it names, which is why losing one is recoverable and losing an
object is not."

## Decision
An engine that keeps its indexes reports which of them have rows missing, and
the layer above fills those before answering a read of one.

`Engine::define` answers a `Defined`. `Engine::rebuilt` records that one is now
complete. Both are on the engine seam because a store is the only place a
previous run's shape was written down.

**Detecting is the engine's and filling is not.** An object is bytes to an
engine, and deriving an index row from one means walking `Record::rows`, which
only the document layer can do. So `doclite-sqlite` notices and records, and
`doclite` fills.

**The gate is in `doclite`, not on `Db`.** `Db::building` reports and waits for
nothing. A gate there could park a byte-level caller with nothing in the world
able to release it, because `doclite-core` cannot derive a row. A caller that
went down a level reads `Db::building` instead.

**Work is parked if and only if it reads a building index.** Filing a row into
one is safe: the row belongs there either way and the fill will not file it
twice. Retiring a range is safe for the same reason. A precondition is a read,
and that is what makes this the load-bearing half: `require_absent` against an
index with rows missing finds a key absent that another document already holds,
so two documents claim one unique key and nothing afterwards can tell them
apart. `Operation::reads` draws the line, and names neither `PutIndex` nor
`DeleteIndex`.

**A page is one transaction and a document is one assertion.** One operation per
document is one commit per document, and a durable engine fsyncs a commit. The
assertion cannot be batched away with it: a writer that supersedes a revision
between the scan and the commit has already retired its rows and filed the new
ones, so filing the scanned revision's rows afterwards leaves a row naming a
revision that is not live, and a read would hand a caller that dead revision as
if it were current. Every document in a page therefore carries `require_current`
over its own run of abort codes, the way a `Transaction` numbers what it stages.

**Where it runs is the caller's.** ADR 0006 keeps the runtime with the caller,
so `Connection::spawning` takes a `Spawner`. `Connection::open` waits instead,
which costs one scan and needs no gate at all.

## Consequences
A connection is opened over record types rather than schemas. `records![User,
Note]` builds the argument, because a schema has forgotten which type it came
from and a rebuild needs the type. `Connection::over` still takes schemas and
refuses to open when something needs filling, which is the honest answer for a
handle that cannot fill anything.

A rebuild that does not finish is an error rather than a wait, and the gate
keeps three reasons apart because a reader is owed the reason and not only the
fact. A `Spawner` may drop the work, and the reads parked on an index nothing
will now fill answer `DocumentError::Rebuilding`. A rebuild that was refused
answers with the refusal instead, so a taken unique key reads the same whether
the fill ran inside `Connection::open` or on a spawner; collapsing the two is
what made one of those doors misreport the other's failure. Neither removes the
entry, because an absent one reads as filled and a read would then answer out of
an index with rows missing.

A `#[unique]` index declared over existing data may already hold two of a key,
because an index carries no constraint (ADR 0001). The fill claims every unique
key it files, so that is `DocumentError::Duplicate` naming the index rather than
two rows nothing can tell apart.

**An existing database is not rebuilt on first open.** One whose index tables
predate any of this has them intact and no record of them, and every write that
touched them kept them in step. `define` asks `sqlite_master` whether the table
is there, and a table with no record is complete.

**Adding a record type to an existing store is ordinary and not a refusal.** Its
generated index is new while the store holds objects, so every index of that
type is reported, and there is nothing to walk because no revision of it exists.
The refusal is narrower: the generated index missing rows while an index derived
from it is not. Nothing here produces that and nothing here can undo it, since
the rows that would say which revisions are live are the ones that are gone.
ADR 0010 refuses an envelope it cannot read for the same reason.

**One state is indistinguishable from a new record type and so is not refused.**
If every index of a type the store does hold records of were dropped out of
band, that is the same evidence a new type presents: an empty generated index on
a store holding objects. Telling them apart needs the objects of that type
enumerated, and the read seam answers scans and point lookups only, so there is
no question to ask. A rebuild takes it as a new type, files nothing, and records
the indexes complete, which leaves those records unfindable and forecloses a
later retry. Nothing in this crate drops an index table, so reaching it means
something else edited the database. It is written down because the consequence
is quiet, which is the one thing the rest of this design refuses to be.

`Store::define` answers a `Defined` too. Nothing a `MemoryStore` holds outlives
the process, so `Defined::ready` is the truth there rather than a convenient
default, and the persistent stores ADR 0011 names as the reason that seam exists
have somewhere to be honest.

**What `define` answers is not a hint.** ADR 0004's table is about what an
engine is told, and ignoring a hint widens what comes back. An engine cannot
ignore its own answer, and one that calls an index complete while its rows are
missing has not saved work. `Probe` forwards it untouched.

### Rejected: a sweep over the generated index
A `for_each` would be atomic with its own scan and would need no assertion at
all. It cannot work: a `Column` is a literal, a column of the scanned row, or
that row's object, so a template can copy a column across but cannot derive one
from an object's body. Deriving a key from a record is exactly what a rebuild
is.
