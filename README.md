# tdsql

`tdsql` is an ergonomic async Microsoft SQL Server client for Rust, built on the
[tiberius-ng](https://github.com/MattJackson/tiberius-ng) TDS driver.

Connect once, then run statements against the connection. Queries hand back rows
directly, and values come out typed:

```rust,no_run
use tdsql::{Client, Config};

#[tokio::main]
async fn main() -> tdsql::Result<()> {
    let mut client = Client::connect(
        &Config::new()
            .host("localhost")
            .port(1433)
            .database("master")
            .auth("sa", "YourStrong!Passw0rd")
            .trust_cert(),
    )
    .await?;

    let rows = client
        .query("SELECT id, name FROM users WHERE id > @P1", &[&10i32])
        .await?;

    for row in &rows {
        let id: i32 = row.get("id");
        let name: String = row.get("name");
        println!("{id}: {name}");
    }

    Ok(())
}
```

## Installation

```toml
[dependencies]
tdsql = "0.1"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

## Connecting

Build the configuration fluently, or parse a connection string:

```rust,no_run
use tdsql::{Client, Config};

# async fn f() -> tdsql::Result<()> {
let config = Config::new()
    .host("localhost")
    .port(1433)
    .database("master")
    .auth("sa", "YourStrong!Passw0rd")
    .trust_cert();

let mut client = Client::connect(&config).await?;

// or, from an ADO.NET-style connection string:
let mut client = Client::connect_str(
    "Server=tcp:localhost,1433;Database=master;User Id=sa;\
     Password=YourStrong!Passw0rd;TrustServerCertificate=true",
)
.await?;
# Ok(())
# }
```

`Config` also accepts a JDBC-style string via [`Config::from_jdbc_string`]. The
password is redacted from the `Debug` output, so logging a config is safe.

One `Client` is one connection, and it is reused for every statement. Because
TDS has no MARS, a connection carries one in-flight request at a time — for
concurrency, use a pool such as [`bb8`](https://crates.io/crates/bb8) or
[`deadpool`](https://crates.io/crates/deadpool) and check out a connection per
task.

## Queries

Parameters are positional: `@P1` is the first, `@P2` the second.

```rust,no_run
# use tdsql::Client;
# async fn f(client: &mut Client) -> tdsql::Result<()> {
// Every row.
let rows = client.query("SELECT id, name FROM users", &[]).await?;

// Exactly one row, or an error.
let row = client
    .query_one("SELECT name FROM users WHERE id = @P1", &[&7i32])
    .await?;

// At most one row.
let maybe = client
    .query_opt("SELECT name FROM users WHERE id = @P1", &[&7i32])
    .await?;

// One value.
let count: i32 = client
    .query_scalar("SELECT COUNT(*) FROM users", &[])
    .await?;

// Rows affected.
let updated = client
    .execute("UPDATE users SET active = 0 WHERE last_login < @P1", &[&"2024-01-01"])
    .await?;
# Ok(())
# }
```

## Reading values

`get` takes a column name or a zero-based position, and converts to the type you
ask for:

```rust,no_run
# use tdsql::Row;
# fn f(row: &Row) -> tdsql::Result<()> {
let id: i32 = row.get("id");
let name: String = row.get(1);

// `try_get` returns an error instead of panicking.
let id: i32 = row.try_get("id")?;

// A nullable column reads as `Option<T>`.
let note: Option<String> = row.try_get("note")?;
# Ok(())
# }
```

Integers widen (a `tinyint` reads fine as `i64`) but never narrow, so a value is
never silently truncated. Reading a `NULL` into a non-optional type is an error
rather than a default value.

Columns keep their order, and `SELECT a, a` stays addressable by position.

## Named parameters and stored procedures

`Command` carries named parameters. A stored procedure is sent as a real RPC, so
its parameters are genuinely named on the wire rather than pasted into an `EXEC`
string:

```rust,no_run
# use tdsql::{Client, Command};
# use rust_decimal::Decimal;
# async fn f(client: &mut Client) -> tdsql::Result<()> {
let cmd = Command::stored_procedure("sp_upsert_order")
    .param("id", 1001)
    .param("status", "PAID")
    .param("amount", Decimal::new(1299, 2));

let affected = client.execute_command(&cmd).await?;
# Ok(())
# }
```

A text batch may also use named parameters; they are rewritten to the positional
form the protocol expects:

```rust,no_run
# use tdsql::{Client, Command};
# async fn f(client: &mut Client) -> tdsql::Result<()> {
let cmd = Command::query("SELECT @id AS id, @flag AS flag")
    .param("id", 7)
    .param("flag", false);

let ds = client.query_dataset(&cmd).await?;
# Ok(())
# }
```

Binding `None` sends SQL `NULL`:

```rust
use tdsql::Command;

let cmd = Command::query("UPDATE users SET note = @note WHERE id = @id")
    .param("note", None::<String>)
    .param("id", 7);
```

## DDL and raw batches

Parameterised statements are sent as an RPC, and some statements — `CREATE
PROCEDURE`, `CREATE VIEW`, `CREATE TRIGGER` — must be the first statement of
their own batch, so they cannot go that route. `batch` sends SQL verbatim:

```rust,no_run
# use tdsql::Client;
# async fn f(client: &mut Client) -> tdsql::Result<()> {
client
    .batch("CREATE PROCEDURE dbo.sp_demo @x INT AS BEGIN SELECT @x; END")
    .await?;
# Ok(())
# }
```

Because the SQL is sent verbatim, never interpolate untrusted input into it —
use parameters for values.

## Multiple result sets

Most commands return one result set, which is why `query` hands back a plain
`Vec<Row>`. When a batch or procedure genuinely returns several, use
`query_dataset` and index them in the order the server sent them:

```rust,no_run
# use tdsql::{Client, Command};
# async fn f(client: &mut Client) -> tdsql::Result<()> {
let ds = client
    .query_dataset(&Command::query("SELECT 1 AS a; SELECT 2 AS b"))
    .await?;

let first: i32 = ds[0][0].get("a");
let second: i32 = ds[1][0].get("b");

// Or look one up by name.
let by_name = ds.table_named("table0");
# Ok(())
# }
```

## Errors

Every fallible call returns [`Error`], so failures can be matched rather than
string-compared:

```rust,no_run
use tdsql::{Client, Error};

# async fn f(client: &mut Client) {
match client.query_one("SELECT name FROM users WHERE id = @P1", &[&7i32]).await {
    Ok(row) => println!("{}", row.get::<String, _>("name")),
    Err(Error::UnexpectedRowCount { found }) => eprintln!("expected 1 row, got {found}"),
    Err(Error::ColumnNotFound(col)) => eprintln!("no column {col}"),
    Err(e) => eprintln!("query failed: {e}"),
}
# }
```

`Error` is `Send + Sync + 'static`, so it also works with `anyhow`, `eyre` and
friends via `?`.

## Type mapping

| SQL Server | `DataValue` | Reads as |
|---|---|---|
| `tinyint` | `TinyInt` | `u8`, `i16`, `i32`, `i64` |
| `smallint` | `SmallInt` | `i16`, `i32`, `i64` |
| `int` | `Int` | `i32`, `i64` |
| `bigint` | `BigInt` | `i64` |
| `real`, `float` | `Float` | `f64` |
| `decimal`, `numeric`, `money` | `Decimal` | `rust_decimal::Decimal` |
| `bit` | `Bool` | `bool` |
| `char`, `varchar`, `nchar`, `nvarchar`, `text`, `xml` | `Text` | `String` |
| `binary`, `varbinary`, `image` | `Binary` | `Vec<u8>` |
| `uniqueidentifier` | `Guid` | `uuid::Uuid` |
| `date` | `Date` | `chrono::NaiveDate` |
| `time` | `Time` | `chrono::NaiveTime` |
| `datetime`, `datetime2`, `smalldatetime` | `DateTime` | `chrono::NaiveDateTime` |
| `datetimeoffset` | `DateTimeOffset` | `chrono::DateTime<FixedOffset>` |
| any `NULL` | `Null` | `Option<T>` |

Any column reads as `DataValue` if you would rather match on it yourself.

### Known limitation: untyped NULLs

Binding `DataValue::Null` (or `None`) sends an untyped `NULL`, which travels as a
`NULL int`. SQL Server coerces that for most targets, but a column that needs a
specific type may reject it. Bind a typed `Option<T>` when that matters.

## Examples

Runnable examples live in [`examples/`](https://github.com/CommonwealthPasta/tdsql/tree/master/examples). The
[`tests`](https://github.com/CommonwealthPasta/tdsql/tree/master/tests) directory holds integration tests that exercise the full type
mapping against a live server.

## License

MIT — see [LICENSE](https://github.com/CommonwealthPasta/tdsql/tree/master/LICENSE).

[`Error`]: https://docs.rs/tdsql/latest/tdsql/enum.Error.html
[`Config::from_jdbc_string`]: https://docs.rs/tdsql/latest/tdsql/struct.Config.html#method.from_jdbc_string
