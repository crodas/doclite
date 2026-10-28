# The struct is the index

The [previous post](01-how-doclite-stores-and-finds-things.md) walked DocLite's
storage layer: objects are bytes under an `ObjectId` you minted, index rows are
byte tuples, and a transaction is a value you build and hand over. That layer is
deliberately ignorant. It holds no schema, enforces no constraint, and cannot
tell an invoice from a photograph.

Which means the caller does all of it. This post is about the layer that stops
that being your problem, worked through a small CRM.

Every sample here is lifted from `crates/doclite/tests/crm.rs`, which passes.

## What it cost by hand

Filing one deal with a unique reference, an index on its account, one on the
amount, and a compound one on the rep and stage:

```rust
static DEAL_BY_REFERENCE: Index<1> = Index::new("deal_by_reference");
static DEAL_BY_ACCOUNT: Index<2> = Index::new("deal_by_account");
static DEAL_BY_AMOUNT_CENTS: Index<2> = Index::new("deal_by_amount_cents");
static DEAL_BY_OWNER_STAGE: Index<3> = Index::new("deal_by_owner_stage");

const DUPLICATE_REFERENCE: AbortCode = AbortCode::new(1000);

let id = mint_an_id_somehow();
let op = Operation::new()
    .require_absent(&DEAL_BY_REFERENCE, exact(key![deal.reference.clone()]), DUPLICATE_REFERENCE)
    .put_object(id, &deal)
    .put_index(&DEAL_BY_REFERENCE, key![deal.reference.clone()], id)
    .put_index(&DEAL_BY_ACCOUNT, key![deal.account, id], id)
    .put_index(&DEAL_BY_AMOUNT_CENTS, key![deal.amount_cents, id], id)
    .put_index(&DEAL_BY_OWNER_STAGE, key![deal.owner, deal.stage, id], id);
```

None of that is wrong. All of it is derivable from the struct, and every line is
one you have to remember to change when a field moves. Moving a deal from
Proposal to Won is worse: retire four index rows, write a new document, file
four more, and hope nobody edited it while you were reading.

## The struct is the specification

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Document)]
#[index(country, city)]
struct Account {
    #[unique]
    tax_id: TaxId,

    name: String,

    country: Country,

    city: String,

    #[index]
    owner: Rep,
}

#[derive(Debug, Clone, Serialize, Deserialize, Document)]
#[index(owner, stage)]
struct Deal {
    #[unique]
    reference: Reference,

    #[index]
    account: ObjectId,

    owner: Rep,

    stage: Stage,

    #[index]
    amount_cents: i64,

    closes_on: Option<String>,
}
```

The indexing attributes are all here. `#[unique]` on a field is an index with a
rule. `#[index]` on a field is one without. `#[index(a, b)]` on the struct is a
compound one. A field with no attribute, like `closes_on`, is stored and not
indexed, which is the common case and needs no ceremony.

The rest go on the struct and say something else entirely. `#[immutable]`
declares that the record has exactly one revision, ever, and editing one is
refused by the compiler rather than by the store. `#[storage(json)]` picks the
encoding its bytes are written in, which is CBOR unless it asks, and is refused
by the compiler on a record carrying a byte field because JSON would write
those as an array of numbers.

What comes out of `Deal`:

| Generated | Shape |
|---|---|
| `Deal::BY_REFERENCE` | `Index<1>` over `(reference)` |
| `Deal::BY_ACCOUNT` | `Index<2>` over `(account, id)` |
| `Deal::BY_AMOUNT_CENTS` | `Index<2>` over `(amount_cents, id)` |
| `Deal::BY_OWNER_STAGE` | `Index<3>` over `(owner, stage, id)` |
| `Deal::schemas()` | those four, for `Connection::open` |
| `Deal::query(&conn)` | a builder with one method per index prefix |
| `impl Record for Deal` | the operations that file, retire and assert |

The columns need to encode as ordered bytes, which for a newtype or a plain enum
is one more line:

```rust
#[derive(Serialize, Deserialize, Column)]
struct TaxId(String);

#[derive(Serialize, Deserialize, Column)]
enum Stage { Lead, Qualified, Proposal, Won, Lost }
```

A newtype delegates to what it wraps. An enum encodes its variant **name**, not
its position. Position would sort by declaration order, which reads nicer right
up until somebody reorders the enum and every index row written so far quietly
means something else. A rename breaks compilation instead.

The cost is that stages sort alphabetically, not by pipeline position:

```rust
stages.sort_by_key(|stage| stage.to_ord_bytes());
assert_eq!(stages, [Stage::Lead, Stage::Proposal, Stage::Qualified, Stage::Won]);
```

So an enum column is for equality. If the order has to mean something, index an
integer next to it.

## The trailing id

Look at the arities again. `amount_cents` is one field and produces an
`Index<2>`.

```rust
let arities: Vec<usize> = Deal::schemas().iter().map(|s| s.arity).collect();
assert_eq!(arities, [1, 2, 2, 3], "only a unique index skips the id");
```

Every non-unique index carries the document id as a trailing column, and that is
forced rather than chosen. `delete_index` takes a *range* and retires everything
in it. There is no delete-this-one-row primitive, because the storage layer
never learned what a row belongs to.

The document rather than the revision, so the key of a row does not move when
the record is superseded and a foreign key written against it keeps finding the
same rows.

So an index keyed on `amount_cents` alone cannot retire one deal's row. Deleting
`exact(key![900_000])` deletes every deal worth 900,000. Putting the id last
makes the key unique per record, so `exact(key![900_000, id])` names exactly one
row.

Unique indexes skip it, and must: `require_absent(exact(key![reference]))` is
what asserts uniqueness, and that only works if the key is the reference and
nothing else. A unique index holds at most one row anyway, so it has nothing to
disambiguate.

Reads are unaffected. `by_amount_cents(900_000)` is a prefix over the first
column of a two-column index, and a prefix selects whole groups.

## The rows that accumulate

One attribute breaks the pattern the rest of this post describes. `#[owner]`
says who a record's changes are about:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Document)]
struct Deal {
    #[unique] reference: Reference,
    #[owner] account: Ref<Account>,
    #[index] amount_cents: i64,
}
```

That declares a feed, keyed on the owner, then the revision, then the document.
The revision in the middle is the point: every other index holds what is
current, and this one holds what happened, in the order it happened, because an
id is minted in time order.

So its rows are not retired when a record is superseded. An edit moves every
other row of that record and adds one here, which is the only place in the
schema where the count grows with the number of edits rather than the number of
records.

The owner is a column like any other. Nothing below the document layer learns
the word, which is the same split as everywhere else: down there it is bytes in
a key, and whether those bytes mean an account is this layer's business.

A `Vec` of owners is the same index again. One revision then files one row per
owner rather than one row, so the change lands in each of their feeds, and the
key still holds exactly one owner because that is what a feed is read for.

## Querying: one method, values and ranges

A `by_` method takes a value or a Rust range, and the argument decides which
comparison it is.

```rust
let large = Deal::query(&conn).by_amount_cents(2_000_000..).get_many().await?;
// [2_000_000, 4_500_000, 12_000_000]

let band = Deal::query(&conn).by_amount_cents(1_000_000..5_000_000).get_many().await?;
// two deals

let strictly_above = Deal::query(&conn)
    .by_amount_cents(greater_than(2_000_000i64))
    .get_many()
    .await?;
// two deals
```

The whole set:

| Written | Means |
|---|---|
| `by_amount_cents(900_000)` | `= 900,000` |
| `by_amount_cents(2_000_000..)` | `>= 2,000,000` |
| `by_amount_cents(..0)` | `< 0` |
| `by_amount_cents(..=0)` | `<= 0` |
| `by_amount_cents(1_000_000..5_000_000)` | `>= 1,000,000` and `< 5,000,000` |
| `by_amount_cents(1_000_000..=5_000_000)` | `>= 1,000,000` and `<= 5,000,000` |
| `by_amount_cents(greater_than(0))` | `> 0` |

That is one method, not seven. The trait behind it looks like it should not
compile:

```rust
pub trait IntoBounds<C> { fn into_bounds(self) -> Bounds; }

impl<C: IntoColumn> IntoBounds<C> for C            // the value
impl<C: IntoColumn> IntoBounds<C> for Range<C>     // the range
```

A blanket over `C` and a blanket over `Range<C>` overlap, normally. They do not
here because the column type is a parameter on the *trait* rather than only on
`Self`. Unifying the two would need `C = Range<C>`, an infinite type, so the
compiler can see they are disjoint. Move `C` onto `Self` alone and it stops
working.

Money is an `i64` of the smallest unit, which means a credit note is a negative
amount, and it has to sort below every sale rather than above them:

```rust
let refunds = Deal::query(&conn).by_amount_cents(..0).get_many().await?;
assert_eq!(refunds[0].amount_cents, -750_000);

let sales = Deal::query(&conn).by_amount_cents(0..).get_many().await?;
assert_eq!(sales.len(), 4);
```

That works because signed integers encode with the sign bit flipped, which turns
two's complement into an unsigned ordering. Without it, `-750_000` would sort
above every positive amount in the index and `..0` would find nothing.

## The bug the type parameter prevents

`greater_than` is the odd one out, because Rust has no exclusive-start range
literal. There is no way to spell `> 0`, so it is a function. The first version
took an untyped value:

```rust
Bounds::greater_than(value: impl IntoColumn)
```

`by_amount_cents(Bounds::greater_than(25))` compiled and returned nothing. No
error, no panic, an empty result.

The literal had nothing pinning its type, so it defaulted to `i32`. An `i32`
encodes as four bytes; the stored column is an `i64`, eight. Comparing them byte
by byte, the four-byte bound sat in the wrong place entirely and matched
nothing.

The fix is to make the value's type follow from the column:

```rust
pub fn greater_than<C: IntoColumn>(value: C) -> Comparison<C>
impl<C: IntoColumn> IntoBounds<C> for Comparison<C>
```

Now `by_amount_cents(greater_than(2_000_000i64))` checks the literal against the
column it is compared to. The typed path never had the hole, because
`IntoBounds<i64>` already pinned it. Only the escape hatch did.

## Compound indexes answer prefixes

`#[index(owner, stage)]` generates two methods, one per prefix:

```rust
let proposals = Deal::query(&conn)
    .by_owner_stage(Rep::Ana, Stage::Proposal)
    .get_many()
    .await?;
assert_eq!(proposals.len(), 1);

let everything_of_anas = Deal::query(&conn).by_owner(Rep::Ana).get_many().await?;
assert_eq!(everything_of_anas.len(), 2);
```

That is a sales pipeline: one rep's deals at one stage, and one rep's whole
book, off the same index. Earlier components are equalities and only the last
takes bounds, which is what an ordered index can actually answer. A range on
`stage` within one rep is a contiguous run of the index; a range on `owner` with
a fixed `stage` is not.

`Account` is the same shape geographically:

```rust
Account::query(&conn).by_country(Country::Paraguay)
Account::query(&conn).by_country_city(Country::Paraguay, "Asuncion".to_string())
```

## Joins are a scan of one index

An `ObjectId` is an index column like any other, so a foreign key is
`#[index] account: ObjectId` and the join is a lookup:

```rust
let theirs = Deal::query(&conn).by_account(roma.id()).get_many().await?;
let references: Vec<&str> = theirs.iter().map(|deal| deal.reference.0.as_str()).collect();
assert_eq!(references, ["DEAL-2026-0001", "DEAL-2026-0002"]);
```

And back the other way, from a deal to the account behind it:

```rust
let found = Deal::query(&conn)
    .by_reference(Reference("DEAL-2026-0001".into()))
    .get_one()
    .await?;

let their_account = Account::query(&conn)
    .by_tax_id(TaxId("80012345-6".into()))
    .get_one()
    .await?;

assert_eq!(found.account, their_account.id());
```

Two reads rather than one join, which is what a store with no query planner
gives you. The previous post's `Query::zip` is there when the two have to agree
with each other.

## Inserting

```rust
conn.insert(deal).await?;
```

One operation: an absence check per unique index, the document, and a row per
index. A taken reference refuses the whole thing, so nothing lands half written.

```rust
assert_eq!(
    conn.insert(duplicate).await,
    Err(DocumentError::Duplicate { index: "deal_by_reference" })
);
assert_eq!(Deal::query(&conn).by_owner(Rep::Carla).get_many().await?.len(), 0);
```

Note that this is an `Err`. The storage layer would call it
`Ok(Outcome::Aborted(code))`, because down there a refused precondition means
the operation did its job. Up here you asked for the insert, so a taken
reference is a failure and `?` should carry it. `Connection` is the one place
that inversion happens, and it also turns the abort code back into the index
name, so you learn *which* unique field was taken rather than that one was.

## Advancing a deal is superseding it

```rust
let mut deal = Deal::query(&conn)
    .by_reference(Reference("DEAL-2026-0001".into()))
    .get_one()
    .await?;

let before = deal.revision();
deal.stage = Stage::Won;
let after = conn.update(deal).await?;

assert_eq!(after.prev(), Some(before));
assert_eq!(after.id(), deal_id, "the same deal, a later revision");
```

There is no read-for-editing mode. A query returns a `Stored<T>` and that is
what an update takes. It derefs to the value, so field access reads normally,
and its `DerefMut` copies the value aside the first time you reach for `&mut`,
unless the record is `#[immutable]`, which is what having no `&mut` to reach for
looks like.
That copy is not bookkeeping: the old index rows are keyed on the old field
values, and retiring them means knowing what they were. Reading a row costs no
copy at all, and editing one costs exactly one.

Two ids come out of this, and the difference is the point. `revision()` is this
version of the deal, and a new one is minted for every edit. `id()` is the deal,
and it never moves. It is the first revision's id, written into every later
revision's envelope, which is why `Deal::account` can hold an `Account`'s `id()`
and keep finding it after the account has been edited.

Every index moves with the deal:

```rust
assert!(Deal::query(&conn)
    .by_owner_stage(Rep::Ana, Stage::Proposal)
    .get_many()
    .await?
    .is_empty());

assert_eq!(
    Deal::query(&conn).by_owner_stage(Rep::Ana, Stage::Won).get_many().await?.len(),
    1
);
```

What `update` builds is exactly what the storage layer's ADR said an update is:
a delete of the old index rows plus an insert of the new ones.

1. assert this revision is still the record's head
2. retire every old index row
3. assert each unique key this value claims is free
4. write the new revision, its `prev` naming the old one
5. file every new index row

The old revision stays where it is. Objects are append-only; nothing rewrites
one, ever.

**Step 3 has to come after step 2.** Put the unique check first and every update
fails as a duplicate of itself, because the deal's own reference row is still
there. That is not a special case bolted on. The executor keeps an overlay of
what the operation has staged, so a precondition placed after a mutation sees
it. Read-your-own-writes, doing the one job it exists for.

**Step 1 is what stops two reps overwriting each other.**

```rust
let mut ana = find().await?;
let mut bruno = find().await?;

bruno.stage = Stage::Won;
conn.update(bruno).await?;

ana.stage = Stage::Lost;
assert_eq!(conn.update(ana).await, Err(DocumentError::Stale));
```

`require_object` would not have caught this, because objects are never deleted
and the old one is still there. What Bruno's update moved is which revision the
record's *head* is, and that is a fact the derive files on purpose: every record
gets a generated `deal_latest` index holding one row per live record, keyed on
the document and the revision. Step 1 counts the row naming Ana's revision, does
not find it, and refuses.

[Nothing is locked](03-nothing-is-locked.md) is the whole story of that check,
and of why the five steps are in that order.

`get_one` refuses to choose, too:

```rust
assert_eq!(
    Deal::query(&conn).by_owner(Rep::Ana).get_one().await,
    Err(DocumentError::NotUnique)
);
assert_eq!(
    Deal::query(&conn).by_reference(Reference("DEAL-9999-9999".into())).get_one().await,
    Err(DocumentError::NotFound)
);
```

Silently editing one of several is a lost write nobody asked a question about.

## History is the audit trail, and it is free

Every CRM builds a deal history table on purpose. Here it is `prev`, walked:

```rust
let mut advancing = find().await?;
advancing.stage = Stage::Proposal;
conn.update(advancing).await?;

let mut closing = find().await?;
closing.stage = Stage::Won;
closing.amount_cents = 1_100_000;
let latest = conn.update(closing).await?;

let history = conn.history(&latest).await?;

let stages: Vec<Stage> = history.iter().map(|deal| deal.stage).collect();
assert_eq!(stages, [Stage::Proposal, Stage::Qualified]);

let amounts: Vec<i64> = history.iter().map(|deal| deal.amount_cents).collect();
assert_eq!(amounts, [900_000, 900_000]);
assert_eq!(latest.amount_cents, 1_100_000);
```

Newest first, back to the original. Every revision is a whole record that was
never touched again, so this is not a reconstruction from a change log, it is a
read. "What did this deal look like in March" and "who moved it to Proposal" are
the same question as "what is it now", asked of a different revision.

Nothing was configured to make that happen. It falls out of objects being
append-only, which was a decision made two layers down for entirely different
reasons.

## Ids and time

`conn.insert(x)` mints an id, which the storage layer flatly refuses to do: an
operation has to be fully described before it runs, so the store can never
invent a value mid-batch. This is the layer that knows what a revision *is*, so
this is where identity comes from.

They are snowflakes:

```
 0 | 41 bits milliseconds | 10 bits node | 12 bits sequence
 ^   since a 2026 epoch                     4096 per ms per node
 sign bit left zero, so an id survives a round trip through i64
```

Ids sort by creation, which means a revision chain and any id-keyed scan read in
the order things happened, for free. `NodeId::derived()` needs no configuration
and `DOCLITE_NODE_ID` pins one when you scale past a single writer. Ten bits is
a real collision risk between derived nodes, and a collision is a duplicate id,
which the store refuses rather than overwrites. It fails loudly.

Time comes from a `Clock`, and it is read once while the operation is built,
never during the walk. That is not tidiness. `Db::run` re-walks the whole
operation on conflict, so a clock read inside the walk would stamp a different
`created_at` on every attempt. Reading it once also means the id's embedded
timestamp and `created_at` can never disagree. A `FixedClock` makes two runs
produce byte-identical revisions, which is what the tests use.

## What the storage layer learned

Nothing.

`doclite-core` has no idea any of this exists. It still holds objects as
opaque bytes, still treats an index as a name over a fixed number of columns
with no rule attached, and its test suite passes with these crates deleted. The
derive emits calls to the same const-generic API you would have written by hand,
which is why an index of the wrong width is still a compile error rather than
something checked again at run time.

That was the point of keeping the bottom layer ignorant. A store that knew about
deals would have had to decide what a deal is. This one decides nothing, and the
layer that does know is a macro you can read.

The one thing this post skipped past is the check in step 1, and what it means
that Ana was told rather than made to wait.
[Nothing is locked](03-nothing-is-locked.md) is about that: how the queries
above and the concurrency control underneath are the same machinery.
