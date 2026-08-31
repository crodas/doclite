# ADR 0002: Order-Preserving Byte Encoding

## Status
Accepted

## Context
An index is ordered, and something has to decide what that order is. If an
engine decides, every engine has to agree, and a compound key over mixed types
becomes a per-engine comparison function nobody can test in one place.

## Decision
The caller encodes. `ToOrdBytes` turns a value into bytes whose `memcmp` order
is the value's own order, and an engine compares those bytes and never
interprets them. Unsigned integers are big-endian; signed integers are the same
with the sign bit flipped, which turns two's complement into an unsigned
ordering; strings and byte strings are themselves.

There is no `Ord` supertrait on `ToOrdBytes`. A type can be encodable without
being comparable in Rust, and requiring both would exclude it for no gain.

`ToOrdBytes` also says what a value *is*, as a `ColumnType` an `IndexSchema`
carries, so a store with a type system of its own can declare a real column
instead of a blob. It is a `ColumnType::Bytes` by default, so a type nobody has
described still encodes rather than being refused, and it settles nothing about
order: the bytes above already did that, and an engine still compares those and
never interprets them.

A compound key is `Vec<Vec<u8>>`, one element per column, never a concatenation.

## Consequences
No escaping is needed. Columns are compared element by element, so one column
cannot bleed into the next, which is the failure mode a concatenated encoding
spends its whole design budget avoiding.

Reading a column back means knowing what it was. `RowKey::component` hands back
bytes, and the layer that wrote them is the one that can decode them.

A partial key compares as a prefix, which is what makes `prefix` and a partial
`between` mean what they look like: they select whole groups of an index.

An endpoint may also be absent. `bounded` takes an `Option` per side and
`KeyRange::contains` treats `None` as satisfied, so `>= 18` and `< 65` are
written without inventing an extreme for the column type. Asking every type for
a maximum was the alternative, and `String` does not have one. `between` is the
two-sided case of `bounded` and nothing else.

`Option<T>` encodes with a leading tag byte, zero for `None` and one for `Some`,
so an absent value sorts before every present one and the two can never be
confused for each other.
