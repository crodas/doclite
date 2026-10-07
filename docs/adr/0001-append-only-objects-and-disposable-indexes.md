# ADR 0001: Append-Only Objects and Disposable Indexes

## Status
Accepted

## Context
A store has to decide what a write is allowed to destroy. The two capabilities
underneath this design are small and pull in opposite directions: an object is
a fact that happened, and an index is a way of finding one. Treating them the
same means either indexes accumulate forever or objects can be overwritten.

## Decision
Objects are inserted and never rewritten. `Batch::objects` carries
insertions only, and an engine refuses an `ObjectId` it already holds with
`StorageError::DuplicateObject`.

Index rows are inserted and deleted freely. `RangeDelete` retires every row of a
range in one step, so a caller never has to enumerate what it is removing.

An index carries no constraint. Whether at most one object may hold a key is a
rule, and rules are asserted by an operation rather than declared on an index.
`IndexSchema` therefore says only which index it is and how wide a key is.

## Consequences
Superseding an object is a delete of the old index rows plus an insert of the
new ones, written out in order, which is spec section 4.2 and not a special
operation.

An index can be rebuilt from the objects it names, which is why losing one is
recoverable and losing an object is not. ADR 0015 is where that is cashed in:
an index whose rows are missing is filled rather than refused. The filling lives
above this crate, because deriving a row from an object means walking
`Record::rows` and an object is bytes here.

An index carrying no constraint has a cost that only shows up there. A
`#[unique]` index declared over data already holding two of a key cannot be
filled, because nothing ever stopped the second one being written. The fill
claims each key it files, so that is a refusal naming the index rather than two
rows nothing can tell apart.

Nothing in this crate ever frees the space an object occupies. A layer above
that wants retirement models it as an ordinary indexed field, because a
tombstone would be erasure wearing an append's clothes.
