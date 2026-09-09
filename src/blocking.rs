//! A blocking client, for callers that are not async.
//!
//! Enable the `blocking` feature to get it:
//!
//! ```toml
//! [dependencies]
//! tdsql = { version = "0.1", features = ["blocking"] }
//! ```
//!
//! It is the same client with the `async` taken off: each call drives the
//! asynchronous one to completion on a runtime the client owns.
//!
//! ```no_run
//! use tdsql::blocking::Client;
//! use tdsql::Config;
//!
//! # fn main() -> tdsql::Result<()> {
//! let mut client = Client::connect(
//!     &Config::new()
//!         .host("localhost")
//!         .database("master")
//!         .auth("sa", "YourStrong!Passw0rd")
//!         .trust_cert(),
//! )?;
//!
//! let rows = client.query("SELECT id, name FROM users WHERE id > @P1", &[&10i32])?;
//! for row in &rows {
//!     let id: i32 = row.get("id");
//!     println!("{id}");
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # Do not call this from async code
//!
//! Each client owns a runtime, and driving a runtime from inside another one
//! panics. [`Client::connect`] detects that case up front and returns
//! [`Error::BlockingInAsync`] rather than letting a later call panic. In async
//! code, use [`tdsql::Client`](crate::Client) directly.

use tokio::runtime::{Builder, Runtime};

use crate::command::Command;
use crate::config::Config;
use crate::dataset::DataSet;
use crate::error::{Error, Result};
use crate::row::Row;
use crate::transaction::IsolationLevel;
use crate::value::{FromSql, ToSql};

fn build_runtime() -> Result<Runtime> {
    // A dedicated worker thread means the connection task keeps making
    // progress between calls, so a queued rollback is sent promptly rather
    // than waiting for the next statement.
    Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .thread_name("tdsql-connection")
        .build()
        .map_err(Error::Io)
}

/// A blocking connection to SQL Server.
///
/// The blocking counterpart to [`tdsql::Client`](crate::Client); every method
/// mirrors it without `async`/`await`.
pub struct Client {
    runtime: Runtime,
    inner: crate::Client,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("blocking::Client")
            .field("inner", &self.inner)
            .finish()
    }
}

impl Client {
    /// Connect using this configuration.
    ///
    /// Fails with [`Error::BlockingInAsync`] if called from inside an async
    /// runtime.
    pub fn connect(config: &Config) -> Result<Self> {
        if tokio::runtime::Handle::try_current().is_ok() {
            return Err(Error::BlockingInAsync);
        }
        let runtime = build_runtime()?;
        let inner = runtime.block_on(crate::Client::connect(config))?;
        Ok(Self { runtime, inner })
    }

    /// Connect using an ADO.NET-style connection string.
    pub fn connect_str(connection_string: &str) -> Result<Self> {
        Self::connect(&Config::from_ado_string(connection_string))
    }

    /// Run a query and collect every row of the first result set.
    pub fn query(&mut self, sql: &str, params: &[&dyn ToSql]) -> Result<Vec<Row>> {
        self.runtime.block_on(self.inner.query(sql, params))
    }

    /// Run a query expecting exactly one row.
    pub fn query_one(&mut self, sql: &str, params: &[&dyn ToSql]) -> Result<Row> {
        self.runtime.block_on(self.inner.query_one(sql, params))
    }

    /// Run a query expecting at most one row.
    pub fn query_opt(&mut self, sql: &str, params: &[&dyn ToSql]) -> Result<Option<Row>> {
        self.runtime.block_on(self.inner.query_opt(sql, params))
    }

    /// Run a query and read the first column of the first row.
    pub fn query_scalar<T: FromSql>(&mut self, sql: &str, params: &[&dyn ToSql]) -> Result<T> {
        self.runtime.block_on(self.inner.query_scalar(sql, params))
    }

    /// Run a statement and report how many rows it affected.
    pub fn execute(&mut self, sql: &str, params: &[&dyn ToSql]) -> Result<u64> {
        self.runtime.block_on(self.inner.execute(sql, params))
    }

    /// Run a SQL batch verbatim, with no parameters, and collect its rows.
    ///
    /// Use this for DDL; see [`tdsql::Client::batch`](crate::Client::batch).
    pub fn batch(&mut self, sql: &str) -> Result<Vec<Row>> {
        self.runtime.block_on(self.inner.batch(sql))
    }

    /// Run a SQL batch verbatim and collect every result set.
    pub fn batch_dataset(&mut self, sql: &str) -> Result<DataSet> {
        self.runtime.block_on(self.inner.batch_dataset(sql))
    }

    /// Run a [`Command`] and collect every result set it produces.
    pub fn query_dataset(&mut self, command: &Command) -> Result<DataSet> {
        self.runtime.block_on(self.inner.query_dataset(command))
    }

    /// Run a [`Command`] for its row count rather than its rows.
    pub fn execute_command(&mut self, command: &Command) -> Result<u64> {
        self.runtime.block_on(self.inner.execute_command(command))
    }

    /// Begin a transaction at the server's default isolation level.
    ///
    /// ```no_run
    /// # use tdsql::blocking::Client;
    /// # fn f(client: &mut Client) -> tdsql::Result<()> {
    /// let mut tx = client.transaction()?;
    /// tx.execute("INSERT INTO orders (id) VALUES (@P1)", &[&1i32])?;
    /// tx.commit()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn transaction(&mut self) -> Result<Transaction<'_>> {
        self.transaction_with_isolation(IsolationLevel::Unspecified)
    }

    /// Begin a transaction at an explicit isolation level.
    pub fn transaction_with_isolation(&mut self, level: IsolationLevel) -> Result<Transaction<'_>> {
        let Self { runtime, inner } = self;
        let tx = runtime.block_on(inner.transaction_with_isolation(level))?;
        Ok(Transaction { runtime, inner: tx })
    }

    /// Close the connection.
    pub fn close(self) -> Result<()> {
        let Self { runtime, inner } = self;
        runtime.block_on(inner.close())
    }
}

/// An in-progress transaction on a blocking [`Client`].
///
/// Behaves exactly like [`tdsql::Transaction`](crate::Transaction): commit to
/// keep the work, and **dropping without committing rolls back**.
pub struct Transaction<'a> {
    runtime: &'a Runtime,
    inner: crate::Transaction<'a>,
}

impl std::fmt::Debug for Transaction<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("blocking::Transaction")
            .field("inner", &self.inner)
            .finish()
    }
}

impl Transaction<'_> {
    /// Commit the work.
    pub fn commit(self) -> Result<()> {
        let Self { runtime, inner } = self;
        runtime.block_on(inner.commit())
    }

    /// Discard the work, reporting any error.
    pub fn rollback(self) -> Result<()> {
        let Self { runtime, inner } = self;
        runtime.block_on(inner.rollback())
    }

    /// Open a nested transaction, backed by an automatically named savepoint.
    pub fn transaction(&mut self) -> Result<Transaction<'_>> {
        let Self { runtime, inner } = self;
        let tx = runtime.block_on(inner.transaction())?;
        Ok(Transaction { runtime, inner: tx })
    }

    /// Open a nested transaction at a named savepoint.
    pub fn savepoint(&mut self, name: impl Into<String>) -> Result<Transaction<'_>> {
        let Self { runtime, inner } = self;
        let tx = runtime.block_on(inner.savepoint(name))?;
        Ok(Transaction { runtime, inner: tx })
    }

    /// Run a query and collect every row of the first result set.
    pub fn query(&mut self, sql: &str, params: &[&dyn ToSql]) -> Result<Vec<Row>> {
        self.runtime.block_on(self.inner.query(sql, params))
    }

    /// Run a query expecting exactly one row.
    pub fn query_one(&mut self, sql: &str, params: &[&dyn ToSql]) -> Result<Row> {
        self.runtime.block_on(self.inner.query_one(sql, params))
    }

    /// Run a query expecting at most one row.
    pub fn query_opt(&mut self, sql: &str, params: &[&dyn ToSql]) -> Result<Option<Row>> {
        self.runtime.block_on(self.inner.query_opt(sql, params))
    }

    /// Run a query and read the first column of the first row.
    pub fn query_scalar<T: FromSql>(&mut self, sql: &str, params: &[&dyn ToSql]) -> Result<T> {
        self.runtime.block_on(self.inner.query_scalar(sql, params))
    }

    /// Run a statement and report how many rows it affected.
    pub fn execute(&mut self, sql: &str, params: &[&dyn ToSql]) -> Result<u64> {
        self.runtime.block_on(self.inner.execute(sql, params))
    }

    /// Run a SQL batch verbatim and collect its rows.
    pub fn batch(&mut self, sql: &str) -> Result<Vec<Row>> {
        self.runtime.block_on(self.inner.batch(sql))
    }

    /// Run a SQL batch verbatim and collect every result set.
    pub fn batch_dataset(&mut self, sql: &str) -> Result<DataSet> {
        self.runtime.block_on(self.inner.batch_dataset(sql))
    }

    /// Run a [`Command`] and collect every result set it produces.
    pub fn query_dataset(&mut self, command: &Command) -> Result<DataSet> {
        self.runtime.block_on(self.inner.query_dataset(command))
    }

    /// Run a [`Command`] for its row count rather than its rows.
    pub fn execute_command(&mut self, command: &Command) -> Result<u64> {
        self.runtime.block_on(self.inner.execute_command(command))
    }
}

mod sealed {
    use tokio::runtime::Runtime;

    /// Grants the default method bodies the runtime and the async connection
    /// underneath.
    ///
    /// Deliberately not public: handing out the inner client from a
    /// `Transaction` would let a caller commit or roll back behind the
    /// transaction's back.
    pub trait Sealed {
        fn parts(&mut self) -> (&Runtime, &mut crate::Client);
    }
}

pub(crate) use sealed::Sealed;

impl Sealed for Client {
    fn parts(&mut self) -> (&Runtime, &mut crate::Client) {
        let Self { runtime, inner } = self;
        (runtime, inner)
    }
}

impl Sealed for Transaction<'_> {
    fn parts(&mut self) -> (&Runtime, &mut crate::Client) {
        let Self { runtime, inner } = self;
        (runtime, inner.client_mut())
    }
}

impl<T: Sealed + ?Sized> Sealed for &mut T {
    fn parts(&mut self) -> (&Runtime, &mut crate::Client) {
        (**self).parts()
    }
}

/// Anything blocking statements can be run against: a [`Client`] or a
/// [`Transaction`].
///
/// The blocking counterpart to [`tdsql::Executor`](crate::Executor). Take
/// `&mut impl Executor` in a helper and the caller decides whether the work
/// lands on the connection directly or inside a transaction:
///
/// ```no_run
/// use tdsql::blocking::{Client, Executor};
/// use tdsql::{Result, Row};
///
/// fn load_user(db: &mut impl Executor, id: i32) -> Result<Row> {
///     db.query_one("SELECT id, name FROM users WHERE id = @P1", &[&id])
/// }
///
/// # fn f(client: &mut Client) -> Result<()> {
/// // Straight to the connection...
/// let user = load_user(client, 1)?;
///
/// // ...or inside a transaction, unchanged.
/// let mut tx = client.transaction()?;
/// let user = load_user(&mut tx, 1)?;
/// tx.commit()?;
/// # Ok(())
/// # }
/// ```
///
/// The trait is sealed — it describes the two types this crate provides rather
/// than an extension point.
pub trait Executor: Sealed {
    /// Open a transaction scoped to this executor.
    ///
    /// On a [`Client`] this begins a real transaction. On a [`Transaction`] it
    /// opens a savepoint instead, so a helper that wants its own atomic scope
    /// nests correctly rather than trying to begin a second transaction on a
    /// connection that already has one.
    fn transaction(&mut self) -> Result<Transaction<'_>>;

    /// Run a query and collect every row of the first result set.
    fn query(&mut self, sql: &str, params: &[&dyn ToSql]) -> Result<Vec<Row>> {
        let (runtime, client) = self.parts();
        runtime.block_on(client.query(sql, params))
    }

    /// Run a query expecting exactly one row.
    fn query_one(&mut self, sql: &str, params: &[&dyn ToSql]) -> Result<Row> {
        let (runtime, client) = self.parts();
        runtime.block_on(client.query_one(sql, params))
    }

    /// Run a query expecting at most one row.
    fn query_opt(&mut self, sql: &str, params: &[&dyn ToSql]) -> Result<Option<Row>> {
        let (runtime, client) = self.parts();
        runtime.block_on(client.query_opt(sql, params))
    }

    /// Run a query and read the first column of the first row.
    fn query_scalar<T: FromSql>(&mut self, sql: &str, params: &[&dyn ToSql]) -> Result<T> {
        let (runtime, client) = self.parts();
        runtime.block_on(client.query_scalar(sql, params))
    }

    /// Run a statement and report how many rows it affected.
    fn execute(&mut self, sql: &str, params: &[&dyn ToSql]) -> Result<u64> {
        let (runtime, client) = self.parts();
        runtime.block_on(client.execute(sql, params))
    }

    /// Run a SQL batch verbatim and collect its rows.
    fn batch(&mut self, sql: &str) -> Result<Vec<Row>> {
        let (runtime, client) = self.parts();
        runtime.block_on(client.batch(sql))
    }

    /// Run a SQL batch verbatim and collect every result set.
    fn batch_dataset(&mut self, sql: &str) -> Result<DataSet> {
        let (runtime, client) = self.parts();
        runtime.block_on(client.batch_dataset(sql))
    }

    /// Run a [`Command`] and collect every result set it produces.
    fn query_dataset(&mut self, command: &Command) -> Result<DataSet> {
        let (runtime, client) = self.parts();
        runtime.block_on(client.query_dataset(command))
    }

    /// Run a [`Command`] for its row count rather than its rows.
    fn execute_command(&mut self, command: &Command) -> Result<u64> {
        let (runtime, client) = self.parts();
        runtime.block_on(client.execute_command(command))
    }
}

impl Executor for Client {
    fn transaction(&mut self) -> Result<Transaction<'_>> {
        Client::transaction(self)
    }
}

impl Executor for Transaction<'_> {
    fn transaction(&mut self) -> Result<Transaction<'_>> {
        Transaction::transaction(self)
    }
}

impl<T: Executor + ?Sized> Executor for &mut T {
    fn transaction(&mut self) -> Result<Transaction<'_>> {
        (**self).transaction()
    }
}
