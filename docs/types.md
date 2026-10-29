# Types

How SQL Server types map to Rust types in `tdsql`, in both directions, and the
corners where the mapping needs a decision from you.

## Type mapping

| SQL Server | `DataValue` | Reads as |
|---|---|---|
| `tinyint` | `TinyInt` | `u8`, `i16`, `i32`, `i64` |
| `smallint` | `SmallInt` | `i16`, `i32`, `i64` |
| `int` | `Int` | `i32`, `i64` |
| `bigint` | `BigInt` | `i64` |
| `real`, `float` | `Float` | `f64`, `f32` |
| `decimal`, `numeric`, `money` | `Decimal` | `rust_decimal::Decimal` |
| `bit` | `Bool` | `bool` |
| `char`, `varchar`, `nchar`, `nvarchar`, `text`, `xml` | `Text` | `String` |
| `binary`, `varbinary`, `image` | `Binary` | `Vec<u8>` |
| `uniqueidentifier` | `Guid` | `uuid::Uuid` |
| `date` | `Date` | `chrono::NaiveDate` |
| `time` | `Time` | `chrono::NaiveTime` |
| `datetime`, `datetime2`, `smalldatetime` | `DateTime` | `chrono::NaiveDateTime` |
| `datetimeoffset` | `DateTimeOffset` | `chrono::DateTime<FixedOffset>`, `DateTime<Utc>`, `DateTime<Local>` |
| any `NULL` | `Null` | `Option<T>` |

Integers can widen but never narrow. A `tinyint` reads fine as an `i64`, and
nothing is ever silently truncated to fit. Reading a `NULL` into a type that
isn't an `Option` is an error.

Columns keep the order the query lists them in. Even `SELECT a, a` gives you two
separate columns, and you can reach each one by position.

You can also read any column as a `DataValue` and match on it yourself. The
enum is non-exhaustive, so give the `match` a `_` arm; a variant can then be
added without a breaking release.

## Getting at `chrono`, `uuid` and `rust_decimal`

Types from these three crates appear in the API, and they only line up with the
exact version `tdsql` was built with. If two copies of `uuid` end up in your
dependency tree, you get the baffling ``expected `Uuid`, found `Uuid` `` error.

To save you from that, `tdsql` re-exports all three. Import them through `tdsql`
and the versions always match:

```rust
use tdsql::chrono::Utc;
use tdsql::uuid::Uuid;
use tdsql::rust_decimal::Decimal;
```

You can still add them to your own `Cargo.toml`. Just keep the major versions in
step with the ones in [`Cargo.toml`](https://github.com/CommonwealthPasta/tdsql/blob/master/Cargo.toml).

## Time zones

You can bind a `chrono::DateTime<Tz>` in any time zone: `Utc`, `Local`, a
`FixedOffset`, or a named zone from `chrono-tz`. It's sent as a `datetimeoffset`
with the zone's offset at that moment, so you get back the same instant you
wrote, whatever the zone:

```rust,no_run
use chrono::{Local, Utc};
use tdsql::{Client, Config};

# async fn run(config: &Config) -> tdsql::Result<()> {
let mut client = Client::connect(config).await?;

client
    .execute("INSERT INTO events (at) VALUES (@P1)", &[&Utc::now()])
    .await?;

let rows = client.query("SELECT at FROM events", &[]).await?;
let at: chrono::DateTime<Utc> = rows[0].get("at");
let same_instant: chrono::DateTime<Local> = rows[0].get("at");
assert_eq!(at, same_instant);
# Ok(())
# }
```

Reading into `DateTime<Utc>` or `DateTime<Local>` converts from the offset the
server sent, and both name the same instant.

`datetime2` and the other offset-free types work differently, in both
directions. They don't store an offset, so treating one as UTC would be a
guess. Those columns bind from and read as `NaiveDateTime`, and you handle the
zone yourself: `.naive_utc()` on the way out, `.and_utc()` on the way back in.
Bind a `DateTime<Local>` or `DateTime<FixedOffset>` into one of them and the
server stores its wall-clock time in that zone, then compares the column with
the parameter as UTC, so `WHERE at >= @P1` can miss the row you just wrote.

## `datetime` columns

A `NaiveDateTime` parameter is sent as `datetime2(7)`, which holds any value it
can carry. The old `datetime` type keeps time to 1/300 of a second, so a column
showing `10:00:00.003` really holds `10:00:00.0033333`, and since database
compatibility level 130 SQL Server converts the column to `datetime2` exactly
before comparing. A value read from a `datetime` column and bound straight back
then fails to match its own row, which turns an optimistic-concurrency `UPDATE
... WHERE updated_at = @P2` into "0 rows affected". Roughly two thirds of
`GETDATE()` values are affected.

For a schema built on `datetime`, send such parameters as `datetime` instead:

```rust
use tdsql::{Config, DateTimeParameterType};

let config = Config::new()
    .host("localhost")
    .auth("sa", "YourStrong!Passw0rd")
    .datetime_parameter_type(DateTimeParameterType::DateTime);
```

They are then rounded to 1/300 s as the column is, and compare as expected. The
trade is that a `datetime2` column written through such a connection gets
1/300 s values too, which is why `datetime2` stays the default.

## NULL parameters

A `NULL` parameter still has a type, and SQL Server checks it against the
column. `None` binds as a `NULL` of its `T`, so `None::<NaiveDate>` goes as a
`date` and `None::<String>` as an `nvarchar`. To bind a `NULL` of a type you
choose, use `DataValue::TypedNull`, such as `DataValue::TypedNull(SqlType::Xml)`.

`DataValue::Null` has no type, so it goes as an `nvarchar`, which SQL Server
converts to every column type except the binary ones. Bind `None::<Vec<u8>>`
for those. A `None` of your own `ToSql` type is untyped too, unless you
implement `ToSql::null_type`.

## Parameter types and query plans

A `&str` or `String` parameter is sent as `nvarchar`. Compared with an indexed
`varchar` column under one of the `SQL_*` collations, which is SQL Server's
default on many installations, that makes the server convert the column rather
than the parameter, and the index is scanned instead of sought. On 300,000
rows that was 20 ms a query instead of 0.4 ms. Bind such a parameter as
`DataValue::VarChar` and it is sent as `varchar` in the server's collation, so
the index is sought:

```rust,no_run
# use tdsql::{Client, DataValue};
# async fn f(client: &mut Client, code: &str) -> tdsql::Result<()> {
let rows = client
    .query(
        "SELECT id FROM items WHERE code = @P1",
        &[&DataValue::VarChar(code.into())],
    )
    .await?;
# Ok(())
# }
```

A character the server's code page cannot hold is an error at bind time, so
keep `VarChar` for columns that are `varchar` themselves. Windows collations
such as `Latin1_General_CI_AS` are not affected.

A `Decimal` parameter is declared as `numeric(38, s)` with the value's own
scale, so each distinct scale compiles its own plan for the same SQL, and the
server sizes arithmetic on it for 38 digits, which cuts a product's scale to 6
places. Round to a fixed scale before binding where that matters.
