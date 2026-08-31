# ADR 0007: Object Ids Are Minted by the Caller

## Status
Accepted

## Context
Something has to hand out identity. If the store does it, then the store decides
what an object is, an insert cannot be described until it has been executed, and
an operation stops being a value that can be built up front and re-run.

## Decision
`ObjectId` is a `u64` the caller mints. The storage layer decides nothing
about where one comes from: monotonicity, node identity and collision avoidance
belong to the layer that knows what an object means.

`ObjectRef` is how a step names one: a literal id, or the object of the row a
`for_each` template is being instantiated for. It is a separate enum from
`Column` so an object slot cannot be filled with an encoded email address.

## Consequences
An operation is fully described before it runs, which is what makes retry free.
An id-generating store would have to invent a value mid-batch and hand it back,
and every step naming it would become a reference to a result.

A caller that wants ordered ids builds them that way. The store never reads a
`ObjectId` as anything but bytes to compare and a key to look under.

Two callers minting the same id is an insert refused with
`StorageError::DuplicateObject`, not a silent overwrite. That is ADR 0001's
rule doing the work here.
