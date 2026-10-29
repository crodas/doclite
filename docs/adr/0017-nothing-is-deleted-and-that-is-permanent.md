# ADR 0017: Nothing Is Deleted, and That Is Permanent

## Status
Accepted

## Context
ADR 0001 made objects append-only and said an index can be rebuilt from the
objects it names. Deletion has been listed ever since under what the design
gives up, with one line of argument: a tombstone is erasure wearing an append's
clothes.

What that list could not say is whether the absence is permanent. A reader
cannot tell "not yet" from "never" from a bullet, and the two lead to different
products. An application that believes a delete is coming models retirement as a
temporary shape and waits. One that knows it is not coming models retirement as
the schema and stops asking.

Both readings are in use. An application built on this store flips a boolean in
ten places because a row cannot go, keeps last month's data because there is
nowhere for it to go, and frees a reservation by rewriting its claim key into a
form nobody queries. Those are not workarounds for a missing feature; each one
is the right shape for an append-only store, and the comments attached to them
say so while still calling the absence a limitation.

The same application also records, in its own release notes, that a seeded
demonstration dataset cannot be removed from a database a real customer would
then use. That is the cost of this decision stated plainly by somebody paying
it, and it is the reason the decision deserves a record of its own rather than a
line in a list of omissions.

## Decision
There is no delete, there will be no delete, and the absence is a guarantee
rather than a gap.

Nothing is added to retract it either. No tombstone, no compaction pass, no
vacuum, no purge tool, no rewrite-into-a-new-file utility shipped as part of
this library. An operation can retire index rows with `delete_index`, because a
row is derived and disposable; the object it named stays.

A caller that wants retirement models it as an ordinary indexed field, which is
what ADR 0001 already said and what the applications already do.

## Consequences
A store only grows. The growth is bounded by what was written rather than by
what is live, so a record edited a thousand times costs a thousand revisions,
and a feed is the one index whose size grows with edits rather than with
records.

Rows written in error are permanent. A seed, an import that ran twice, a
customer created during a demonstration: each is in the file for as long as the
file exists. The remedy is outside this library, which means starting a new
store and writing into it only what should be there. Nothing here will help with
that, on purpose, because a tool that rewrites a store is the delete this record
refuses dressed as maintenance.

Erasure cannot be promised to anybody. A caller with a legal or contractual
obligation to destroy data on request cannot discharge it through this store,
and should not choose it for data carrying that obligation. Encrypting a record
and discarding the key is the usual answer and is the caller's to build; this
library has no opinion about it and no support for it.

The guarantee is worth something in return. A revision is never rewritten, so a
hash chain over revisions holds without a second mechanism, an audit trail
cannot be edited by the thing being audited, and `history` answers for the whole
life of a record rather than for the part nobody has tidied. An application
holding an append-only ledger beside this store gets agreement between them for
free, where a store that could delete would need the two kept in step.

This closes the question rather than scheduling it. A later record may
contradict this one, which is how a living document works, but it will have to
argue against the guarantee rather than fill a gap.
