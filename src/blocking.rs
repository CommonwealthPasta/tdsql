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
