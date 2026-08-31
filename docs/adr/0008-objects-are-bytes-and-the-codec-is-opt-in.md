# ADR 0008: Objects Are Bytes, and the Codec Is Opt In

## Status
Accepted

## Context
An object is bytes to the engine and almost always a struct to the caller.
Something has to sit between them, and where it sits decides two things: whether
the store can ever interpret an object, and whether a caller is stuck with one
encoding.

## Decision
`ToStorage` and `FromStorage` are the two halves of that seam, and they name no
format. `Vec<u8>` implements both as the identity, so the byte-level API is a
use of the traits rather than a thing beside them, and
`Operation::put_object` takes an `impl ToStorage` rather than a `Vec<u8>`.

There are two formats, each behind its own feature and each reached through a
macro rather than a blanket impl over `Serialize`. A blanket would overlap the
identity impls, which Rust has no negative reasoning to separate, and it would
leave no room for a type that wants an encoding of its own. One line per type
keeps both.

`cbor` brings `serde_storage!`, and it is the default. `json` brings
`json_storage!`. Neither is a smaller format, and that is the requirement rather
than an accident: ADR 0001 makes objects append-only, so an object written
before a struct gained a field is never rewritten and has to still decode
afterwards. Only a self-describing encoding gives that, and both of these are
one.

What separates them is who else can read the store. CBOR is compact and opaque
to everything that is not this crate. JSON is bigger and legible, and under
`doclite-sqlite` that legibility reaches all the way down: such a record lands
in `doclite_json_document` as a text value, so `sqlite3` prints the document and
`json_extract` reads a field out of it without decoding anything first.

Legibility only holds if the *whole* object is JSON, so `Revision<T>` has a
second framing. A binary header in front of a JSON body would leave the object
neither one thing nor the other, so a text record gets a JSON envelope instead,
with the record nested under `body` rather than flattened beside the envelope's
own fields. Nesting is what makes a record free to declare a field named `doc`
or `prev`. Which framing to write is `ToStorage::TEXT`, a promise about the
bytes that the codec macro sets and the envelope reads.

The choice is per record, written `#[storage(json)]` on the struct, because it
is a fact about that record rather than about the store. Two records in one
database may disagree, and the features are additive so both codecs can be
compiled in at once.

`#[storage(json)]` is refused at compile time on a record with a byte field,
naming the field. `serde_json` encodes a `Vec<u8>` as an array of numbers, which
round-trips correctly and is unreadable, so a record stored that way would have
paid JSON's size for none of its benefit. The check reads the field's written
type, so an alias or a newtype around `Vec<u8>` goes through; it catches what
gets written rather than everything that could be.

`put_object` encodes when the operation is built, not when it runs. A value
that cannot encode is a defect in the operation, so it is kept as
`BuildError::Encode` and reported by `Operation::check` before an engine is
asked anything. The failure is stored on the operation rather than returned, so
a builder chain stays one expression.

## Consequences
The engine trait does not change and cannot. `Batch::objects` still carries
bytes, an engine still never interprets one, and ADR 0004's contract is
untouched.

An object read back as the wrong type is `StorageError::Decode`, not a wrong
answer. The read reached the store correctly and the caller named the wrong
shape, which is why it is a separate variant from `ReadMismatch`.

Three features now mention serde and mean different things. `serde` derives
`Serialize` on the operation representation so an operation could be shipped,
per ADR 0003; `cbor` and `json` are the two object codecs. None implies another.

An engine still learns nothing from any of this. `doclite-sqlite` keeps two
tables, `doclite_document` and `doclite_json_document`, and picks between them
on whether the bytes are UTF-8. That is a fact about the bytes rather than about
the codec that made them, so the engine is never told which record chose what.
Two tables rather than two storage classes in one column, because ADR 0002's
ordering argument makes a mixed column a thing this design refuses everywhere
else and objects are not worth an exception.

An id therefore names one object across two tables. Every object read is a
`UNION ALL` over both, and the duplicate rule is an explicit `has_object` before
filing rather than a primary-key conflict, which cannot see across tables.

`Query::object` hangs off `Query<()>` and takes its type as a method
parameter, because it is the one read whose output is not fixed by its
arguments. `limit` moved to `Query<Vec<Row>>` for a duller reason: with
objects generic, `Vec<(Row, T)>` and `Vec<(Row, Option<T>)>` unify at
`T = Option<U>`, and an inherent method cannot be defined on both.
