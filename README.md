# tdsql

`tdsql` is an async Microsoft SQL Server client for Rust, built on the
[tds-driver](https://github.com/CommonwealthPasta/tds-driver) TDS driver.

You connect once and run statements on that connection. Queries return rows,
and you read values out of them as ordinary Rust types:

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
tdsql = "0.7.0"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

## Connecting

Build a `Config` in code, or parse a connection string:

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

[`Config::from_jdbc_string`] parses JDBC-style strings too. A config's `Debug`
output hides the password, so it's safe to log.

Connecting gives up after 15 seconds unless the config says otherwise: use
`Config::connect_timeout`, or `Connect Timeout` in a connection string, where 0
means no limit. A named instance without a port, such as `Server=host\SQLEXPRESS`,
is found through SQL Server Browser. If the server redirects the login, as Azure
SQL's gateway does, `connect` follows it.

A `Client` is one connection, reused for every statement. The driver doesn't
support MARS, so a connection runs one request at a time. For concurrency, use a
pool such as [`bb8`](https://crates.io/crates/bb8) or
[`deadpool`](https://crates.io/crates/deadpool) and give each task its own
connection.

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

`get` takes a column name or a zero-based position, and converts the value to
the type you ask for:

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

Integers can widen but never narrow. A `tinyint` reads fine as an `i64`, and
nothing is ever silently truncated to fit. Reading a `NULL` into a type that
isn't an `Option` is an error.

Columns keep the order the query lists them in. Even `SELECT a, a` gives you two
separate columns, and you can reach each one by position.

## Streaming large results

`query` gives you nothing until the last row has arrived. `query_stream` hands
you each row as soon as the server sends it:

```rust,no_run
# use tdsql::Client;
# async fn f(client: &mut Client) -> tdsql::Result<()> {
let mut rows = client
    .query_stream("SELECT id, name FROM events WHERE day = @P1", &[&"2024-06-01"])
    .await?;

// The columns are known before the first row arrives.
for column in rows.columns() {
    println!("{}: {:?}", column.name(), column.sql_type());
}

while let Some(row) = rows.try_next().await? {
    let id: i64 = row.get("id");
    println!("{id}");
}
# Ok(())
# }
```

You get the rows of the first result set, the same ones `query` would collect.
If the server rejects the statement, the `.await` on `query_stream` fails. If
something goes wrong partway through, the error comes out of the stream after
the rows that made it. `RowStream` is also a `futures::Stream`, if you'd rather
use combinators.

The stream borrows the client, so nothing else can run on the connection while
you hold it. Rows are read only a little ahead of you. A slow reader slows the
query down, and memory use stays flat however big the result is. The query stays
open on the server until you're done with it, and keeps any locks it has taken.

You can drop the stream early. That cancels the query on the server, and the
connection is ready for the next statement as soon as the server has confirmed.
A query still producing rows stops there, along with whatever follows it in the
batch. A small result was over before the cancel arrived, so the rest of its
batch ran.

In the blocking client, `query_stream` returns an `Iterator`.

## Cancelling

Stop waiting for a request and it's cancelled on the server too. Drop its
future, let `tokio::time::timeout` or `select!` give up on it, abort the task
running it, or drop a stream early. The server stops working on it and lets go
of any locks it took, and the connection stays usable:

```rust,no_run
# use std::time::Duration;
# use tdsql::Client;
# async fn f(client: &mut Client) -> tdsql::Result<()> {
let report = tokio::time::timeout(
    Duration::from_secs(5),
    client.query("SELECT region, SUM(total) FROM orders GROUP BY region", &[]),
)
.await;

if report.is_err() {
    // Timed out and cancelled, so this runs straight away rather than
    // queueing behind the abandoned query.
    let count: i32 = client.query_scalar("SELECT COUNT(*) FROM orders", &[]).await?;
    println!("{count} orders; the report timed out");
}
# Ok(())
# }
```

A request still waiting its turn behind another never runs. One that's running
stops where it is, even if it hasn't sent anything back yet because it's still
sorting, aggregating or waiting on a lock.

Commits and rollbacks always run, whatever became of their futures. Cancelling
stops a statement, not the transaction around it: work that finished before the
cancel stays done, just as it would if the statement had failed. Put statements
that must stand or fall together in a transaction. The exception is a session
with `SET XACT_ABORT ON`, where the server treats a cancel like any other
statement failure and rolls the whole transaction back.

## Named parameters and stored procedures

`Command` takes named parameters. Stored procedures are called over RPC, which
sends each parameter by name, so nothing gets pasted into an `EXEC` string:

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

Text queries can use named parameters too. They're rewritten into the
positional form the protocol expects. Names match whatever their case, so
`@Id` in the SQL finds a parameter bound as `id`:

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

To send SQL `NULL`, bind `None`:

```rust
use tdsql::Command;

let cmd = Command::query("UPDATE users SET note = @note WHERE id = @id")
    .param("note", None::<String>)
    .param("id", 7);
```

## Transactions

`client.transaction()` starts a transaction, and everything you run through the
handle it returns is part of it. Call `commit()` to keep the work. If you drop
the handle without committing, the transaction rolls back.

```rust,no_run
# use tdsql::Client;
# async fn f(client: &mut Client) -> tdsql::Result<()> {
let mut tx = client.transaction().await?;

tx.execute("INSERT INTO orders (id) VALUES (@P1)", &[&1i32]).await?;
tx.execute("UPDATE stock SET qty = qty - 1 WHERE id = @P1", &[&7i32]).await?;

tx.commit().await?;   // or tx.rollback().await?
# Ok(())
# }
```

Sometimes the server ends a transaction itself: a deadlock, an error with
`SET XACT_ABORT ON`, or a `ROLLBACK` inside a procedure you called. The handle
notices. Any further statement on it fails with `Error::TransactionAborted`
rather than running outside the transaction, `commit()` fails the same way,
and `rollback()` succeeds, since there is nothing left to undo.

Nothing commits unless you ask it to, so returning early with `?` is safe. Here
the second insert fails, and the first one is rolled back with it:

```rust,no_run
# use tdsql::Client;
async fn transfer(client: &mut Client) -> tdsql::Result<()> {
    let mut tx = client.transaction().await?;
    tx.execute("INSERT INTO orders (id) VALUES (@P1)", &[&1i32]).await?;
    tx.execute("INSERT INTO nonexistent VALUES (1)", &[]).await?;  // fails
    tx.commit().await
}
```

You can pass a `&mut Transaction` to other functions, so one unit of work can
span several of them:

```rust,no_run
use tdsql::{Client, Transaction};

async fn add_order(tx: &mut Transaction<'_>, id: i32) -> tdsql::Result<()> {
    tx.execute("INSERT INTO orders (id) VALUES (@P1)", &[&id]).await?;
    Ok(())
}

# async fn f(client: &mut Client) -> tdsql::Result<()> {
let mut tx = client.transaction().await?;
add_order(&mut tx, 1).await?;
add_order(&mut tx, 2).await?;
tx.commit().await?;
# Ok(())
# }
```

You can also choose an isolation level, which lasts until the transaction
ends, and use savepoints. A savepoint works like a nested transaction.
Rolling it back undoes its own work and leaves the outer transaction open.

```rust,no_run
# use tdsql::{Client, IsolationLevel};
# async fn f(client: &mut Client) -> tdsql::Result<()> {
let mut tx = client
    .transaction_with_isolation(IsolationLevel::Serializable)
    .await?;

{
    let mut sp = tx.savepoint("before_risky").await?;
    sp.execute("DELETE FROM audit WHERE id = @P1", &[&5i32]).await?;
    sp.rollback().await?;      // undone; `tx` is still live
}

tx.commit().await?;
# Ok(())
# }
```

### How rollback-on-drop works

`Drop` can't run async code, so it can't wait for a rollback to finish. It
queues one instead, which never blocks. The connection runs in a background task
that handles requests in order, so the rollback reaches the server before
anything else you send on that connection. If you need to know whether the
rollback itself failed, call `rollback()` yourself.

## Writing helpers that work either way

A helper that takes `&mut Client` can't be called inside a transaction, and one
that takes `&mut Transaction` can't be called outside one. Take
`&mut impl Executor` instead, and it works with both:

```rust,no_run
# use tdsql::{Client, Executor, Result, Row};
async fn load_user(db: &mut impl Executor, id: i32) -> Result<Row> {
    db.query_one("SELECT id, name FROM users WHERE id = @P1", &[&id])
        .await
}

# async fn f(client: &mut Client) -> Result<()> {
// Straight to the connection...
let user = load_user(client, 1).await?;

// ...or inside a transaction, unchanged.
let mut tx = client.transaction().await?;
let user = load_user(&mut tx, 1).await?;
tx.commit().await?;
# Ok(())
# }
```

`Executor` has the same statement methods as `Client`, so you can make an
existing helper generic by changing nothing but its signature.

It also has `transaction()`. On a `Client` that starts a transaction, and on a
`Transaction` it opens a savepoint. So a helper can open a scope of its own
without knowing whether it's already inside a transaction:

```rust,no_run
# use tdsql::{Client, Executor, Result};
/// Rolls back its own work without disturbing whatever it was called from.
async fn try_it(db: &mut impl Executor) -> Result<()> {
    let mut scope = db.transaction().await?;
    scope.execute("INSERT INTO audit (id) VALUES (@P1)", &[&1i32]).await?;
    scope.rollback().await
}

# async fn f(client: &mut Client) -> Result<()> {
// Here the scope is a transaction, and rolling it back discards the insert.
try_it(client).await?;

// Here it is a savepoint: the insert is discarded, the outer work is not.
let mut tx = client.transaction().await?;
tx.execute("INSERT INTO orders (id) VALUES (@P1)", &[&1i32]).await?;
try_it(&mut tx).await?;
tx.commit().await?;
# Ok(())
# }
```

The blocking client has its own version of the trait, `tdsql::blocking::Executor`.

`Executor` is sealed, so `Client` and `Transaction` are the only types that
implement it. It isn't dyn-compatible either: write `&mut impl Executor` or
`<E: Executor>`, not `&mut dyn Executor`.

## DDL and raw batches

A statement with parameters is sent through `sp_executesql`, which runs it in a
scope of its own: a `SET` option it changes is undone when it returns. One with
no parameters is sent as a plain batch, as SqlClient does, so
`execute("SET XACT_ABORT ON", &[])` applies to the session. Some statements,
like `CREATE PROCEDURE`, `CREATE VIEW` and `CREATE TRIGGER`, have to come first
in a batch of their own, so they can't take parameters at all. `batch` sends SQL
exactly as you wrote it and returns every row of every result set:

```rust,no_run
# use tdsql::Client;
# async fn f(client: &mut Client) -> tdsql::Result<()> {
client
    .batch("CREATE PROCEDURE dbo.sp_demo @x INT AS BEGIN SELECT @x; END")
    .await?;
# Ok(())
# }
```

Since the SQL goes over untouched, never build it from untrusted input. Values
belong in parameters.

## Multiple result sets

Most statements return a single result set, which is why `query` returns a plain
`Vec<Row>`. When a batch or procedure returns several, use `query_dataset`.
They come back in the order the server sent them:

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

Everything that can fail returns an [`Error`], so you can match on what went
wrong:

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

When the server rejects a statement, you get `Error::Query`. Its message names
the statement and repeats what the server said, so printing it is usually all
you need:

```text
query failed [SELECT id FROM nope]: Invalid object name 'nope'. [error 208, state 1, severity 16, line 1]
```

The same details are available as fields. Use those to handle a specific server
error, so you never have to parse the message:

```rust,no_run
# use tdsql::Client;
# async fn f(client: &mut Client) {
if let Err(e) = client.execute("INSERT INTO users (id) VALUES (@P1)", &[&7i32]).await {
    eprintln!("{} failed", e.statement().unwrap_or("statement"));
    if let (Some(code), Some(message)) = (e.code(), e.server_message()) {
        eprintln!("  server said {code}: {message} (line {})", e.line().unwrap_or(0));
    }
    if e.is_deadlock() {
        // Deadlock victim (error 1205): the transaction is gone, retry it.
    }
}
# }
```

Parameter values never appear in error messages, since they often hold personal
data. `parameter_count()` tells you how many were bound, and `statement_kind()`
tells you whether the failure came from a query, a batch, a stored procedure or
transaction control.

`Error` is `Send + Sync + 'static`, so `?` works with `anyhow`, `eyre` and
similar crates.

## Blocking client

If your program isn't async, turn on the `blocking` feature:

```toml
[dependencies]
tdsql = { version = "0.7.0", features = ["blocking"] }
```

`tdsql::blocking::Client` is the async client without the `async` and `.await`.
Typed rows, `Command`, transactions, savepoints and the `Executor` trait all work
the same way, and `Row`, `DataSet` and `Error` are shared between the two. The
only difference in shape is `query_stream`, which returns an `Iterator` instead
of a `Stream`.

```rust,no_run
use tdsql::blocking::Client;
use tdsql::Config;

fn main() -> tdsql::Result<()> {
    let mut client = Client::connect(
        &Config::new()
            .host("localhost")
            .database("master")
            .auth("sa", "YourStrong!Passw0rd")
            .trust_cert(),
    )?;

    let row = client.query_one("SELECT @P1 AS n", &[&42i32])?;
    let n: i32 = row.get("n");

    let mut tx = client.transaction()?;
    tx.execute("INSERT INTO orders (id) VALUES (@P1)", &[&1i32])?;
    tx.commit()?;

    Ok(())
}
```

Each blocking client runs its own small runtime, so don't use one from async
code. Starting a runtime inside another one panics. `Client::connect` checks for
this and returns `Error::BlockingInAsync`, so you find out when you connect, not
on some later call. In async code, use `tdsql::Client`.

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

You can also read any column as a `DataValue` and match on it yourself. The
enum is non-exhaustive, so give the `match` a `_` arm; a variant can then be
added without a breaking release.

### Getting at `chrono`, `uuid` and `rust_decimal`

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

### Time zones

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

### NULL parameters

A `NULL` parameter still has a type, and SQL Server checks it against the
column. `None` binds as a `NULL` of its `T`, so `None::<NaiveDate>` goes as a
`date` and `None::<String>` as an `nvarchar`. To bind a `NULL` of a type you
choose, use `DataValue::TypedNull`, such as `DataValue::TypedNull(SqlType::Xml)`.

`DataValue::Null` has no type, so it goes as an `nvarchar`, which SQL Server
converts to every column type except the binary ones. Bind `None::<Vec<u8>>`
for those. A `None` of your own `ToSql` type is untyped too, unless you
implement `ToSql::null_type`.

## Examples

There are runnable examples in [`examples/`](https://github.com/CommonwealthPasta/tdsql/tree/master/examples).
The [`tests`](https://github.com/CommonwealthPasta/tdsql/tree/master/tests)
directory has integration tests that run the full type mapping against a live
server.

## License

MIT. See [LICENSE](https://github.com/CommonwealthPasta/tdsql/tree/master/LICENSE).

[`Error`]: https://docs.rs/tdsql/latest/tdsql/enum.Error.html
[`Config::from_jdbc_string`]: https://docs.rs/tdsql/latest/tdsql/struct.Config.html#method.from_jdbc_string
