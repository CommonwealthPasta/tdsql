# How it works

What `tdsql` does on the connection, for when the behaviour matters: how
requests are ordered, what cancelling and streaming do on the server, how a
dropped transaction rolls back, and the rules for the blocking client.

## Connections

A `Client` is one connection, reused for every statement. The driver doesn't
support MARS, so a connection runs one request at a time. Requests sent while
another is running wait their turn, in order. For concurrency, use a pool such
as [`bb8`](https://crates.io/crates/bb8) or
[`deadpool`](https://crates.io/crates/deadpool) and give each task its own
connection.

Connecting gives up after 15 seconds unless the config says otherwise: use
`Config::connect_timeout`, or `Connect Timeout` in a connection string, where 0
means no limit. A named instance without a port, such as `Server=host\SQLEXPRESS`,
is found through SQL Server Browser. If the server redirects the login, as Azure
SQL's gateway does, `connect` follows it.

A config's `Debug` output hides the password, so it's safe to log.

When the server drops the connection, because an administrator ran `KILL`,
the network went away, or a severity 20 error closed the session, the
statement that finds it gone fails with an `Error::Query` whose source is the
I/O error, naming the statement and the cause. The connection then closes,
`Client`'s `Debug` output shows `open: false`, and every later statement fails
with `Error::ConnectionClosed`. `Error::is_connection_lost()` is true for both,
which is what a pool should check before handing the client out again.

## Statements and batches

A statement with parameters is sent through `sp_executesql`, which runs it in a
scope of its own: a `SET` option it changes is undone when it returns. One with
no parameters is sent as a plain batch, as SqlClient does, so
`execute("SET XACT_ABORT ON", &[])` applies to the session. `batch` always
sends the SQL as a plain batch, untouched, and returns every row of every
result set.

Named parameters in a text query are rewritten into the positional form the
protocol expects before the statement is sent. Stored procedures go over RPC,
which carries each parameter by name.

## Streaming

`query_stream` gives you the rows of the first result set, the same ones
`query` would collect. If the server rejects the statement, the `.await` on
`query_stream` fails. If something goes wrong partway through, the error comes
out of the stream after the rows that made it.

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

## Cancellation

Dropping a request's future, timing it out, aborting the task running it, or
dropping a stream early all cancel the request on the server. A request still
waiting its turn behind another never runs. One that's running stops where it
is, even if it hasn't sent anything back yet because it's still sorting,
aggregating or waiting on a lock. The server lets go of any locks it took, and
the connection stays usable.

Commits and rollbacks always run, whatever became of their futures. Cancelling
stops a statement, not the transaction around it: work that finished before the
cancel stays done, just as it would if the statement had failed. Put statements
that must stand or fall together in a transaction. The exception is a session
with `SET XACT_ABORT ON`, where the server treats a cancel like any other
statement failure and rolls the whole transaction back.

## Transactions

Sometimes the server ends a transaction itself: a deadlock, an error with
`SET XACT_ABORT ON`, or a `ROLLBACK` inside a procedure you called. The handle
notices. Any further statement on it fails with `Error::TransactionAborted`
rather than running outside the transaction, `commit()` fails the same way,
and `rollback()` succeeds, since there is nothing left to undo.

A savepoint works like a nested transaction. Rolling it back undoes its own
work and leaves the outer transaction open. An isolation level chosen with
`transaction_with_isolation` lasts until the transaction ends.

### Rollback on drop

`Drop` can't run async code, so it can't wait for a rollback to finish. It
queues one instead, which never blocks. The connection runs in a background task
that handles requests in order, so the rollback reaches the server before
anything else you send on that connection. If you need to know whether the
rollback itself failed, call `rollback()` yourself.

## The `Executor` trait

`Executor` has the same statement methods as `Client` and `Transaction`, plus
`transaction()`, which starts a transaction on a `Client` and opens a savepoint
on a `Transaction`. So a helper can open a scope of its own without knowing
whether it's already inside a transaction:

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

## Errors

When the server rejects a statement, you get `Error::Query`. Its message names
the statement and repeats what the server said:

```text
query failed [SELECT id FROM nope]: Invalid object name 'nope'. [error 208, state 1, severity 16, line 1]
```

The same details are available as fields: `code()`, `server_message()`,
`line()`, `statement()` and `is_deadlock()`. Parameter values never appear in
error messages, since they often hold personal data. `parameter_count()` tells
you how many were bound, and `statement_kind()` tells you whether the failure
came from a query, a batch, a stored procedure or transaction control.

## The blocking client

Each blocking client runs its own small runtime, so don't use one from async
code. Driving a runtime from a thread that is running another one panics.
`Client::connect` refuses when a Tokio runtime is current on the calling thread
and returns `Error::BlockingInAsync`; that includes `spawn_blocking` closures,
where the client would in fact work, so connect on a thread of your own and move
it in. The check is made at connect only: a client *used* inside async code
panics as Tokio does, though dropping one there is fine. In async code, use
`tdsql::Client`.
