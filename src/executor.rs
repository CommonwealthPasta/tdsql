//! Writing code that does not care where its statements run.
//!
//! [`Executor`] is implemented by [`Client`] and [`Transaction`], so a helper
//! can take `&mut impl Executor` and be called with either:
//!
//! ```no_run
//! use tdsql::{Executor, Result, Row};
//!
//! async fn load_user(db: &mut impl Executor, id: i32) -> Result<Row> {
//!     db.query_one("SELECT id, name FROM users WHERE id = @P1", &[&id])
//!         .await
//! }
//!
//! # async fn f(client: &mut tdsql::Client) -> Result<()> {
//! // Straight to the connection...
//! let user = load_user(client, 1).await?;
//!
//! // ...or inside a transaction, unchanged.
//! let mut tx = client.transaction().await?;
//! let user = load_user(&mut tx, 1).await?;
//! tx.commit().await?;
//! # Ok(())
//! # }
//! ```

use std::future::Future;

use crate::client::Client;
use crate::command::Command;
use crate::dataset::DataSet;
use crate::error::Result;
use crate::row::Row;
use crate::transaction::Transaction;
use crate::value::{FromSql, ToSql};

mod sealed {
    /// Grants the default method bodies access to the underlying connection.
    ///
    /// This is deliberately not public: handing out `&mut Client` from a
    /// `Transaction` would let a caller commit or roll back behind the
    /// transaction's back.
    pub trait Sealed {
        fn as_client(&mut self) -> &mut crate::client::Client;
    }
}

pub(crate) use sealed::Sealed;

impl Sealed for Client {
    fn as_client(&mut self) -> &mut Client {
        self
    }
}

impl Sealed for Transaction<'_> {
    fn as_client(&mut self) -> &mut Client {
        self.client_mut()
    }
}

impl<T: Sealed + ?Sized> Sealed for &mut T {
    fn as_client(&mut self) -> &mut Client {
        (**self).as_client()
    }
}

/// Anything statements can be run against: a [`Client`] or a [`Transaction`].
///
/// Take `&mut impl Executor` in a helper and the caller decides whether the
/// work lands on the connection directly or inside a transaction. The trait is
/// sealed — it describes the two types this crate provides rather than an
/// extension point.
///
/// Every method mirrors the inherent method of the same name, so an existing
/// helper written against `&mut Client` becomes generic by changing only its
/// signature.
pub trait Executor: Sealed + Send {
    /// Open a transaction scoped to this executor.
    ///
    /// On a [`Client`] this begins a real transaction. On a [`Transaction`] it
    /// opens a savepoint instead, so a helper that wants its own atomic scope
    /// nests correctly rather than trying to begin a second transaction on a
    /// connection that already has one.
    fn transaction(&mut self) -> impl Future<Output = Result<Transaction<'_>>> + Send;

    /// Run a query and collect every row of the first result set.
    fn query(
        &mut self,
        sql: &str,
        params: &[&dyn ToSql],
    ) -> impl Future<Output = Result<Vec<Row>>> + Send {
        self.as_client().query(sql, params)
    }

    /// Run a query expecting exactly one row.
    fn query_one(
        &mut self,
        sql: &str,
        params: &[&dyn ToSql],
    ) -> impl Future<Output = Result<Row>> + Send {
        self.as_client().query_one(sql, params)
    }

    /// Run a query expecting at most one row.
    fn query_opt(
        &mut self,
        sql: &str,
        params: &[&dyn ToSql],
    ) -> impl Future<Output = Result<Option<Row>>> + Send {
        self.as_client().query_opt(sql, params)
    }

    /// Run a query and read the first column of the first row.
    fn query_scalar<T: FromSql + Send>(
        &mut self,
        sql: &str,
        params: &[&dyn ToSql],
    ) -> impl Future<Output = Result<T>> + Send {
        self.as_client().query_scalar(sql, params)
    }

    /// Run a statement and report how many rows it affected.
    fn execute(
        &mut self,
        sql: &str,
        params: &[&dyn ToSql],
    ) -> impl Future<Output = Result<u64>> + Send {
        self.as_client().execute(sql, params)
    }

    /// Run a SQL batch verbatim and collect its rows.
    fn batch(&mut self, sql: &str) -> impl Future<Output = Result<Vec<Row>>> + Send {
        self.as_client().batch(sql)
    }

    /// Run a SQL batch verbatim and collect every result set.
    fn batch_dataset(&mut self, sql: &str) -> impl Future<Output = Result<DataSet>> + Send {
        self.as_client().batch_dataset(sql)
    }

    /// Run a [`Command`] and collect every result set it produces.
    fn query_dataset(&mut self, command: &Command) -> impl Future<Output = Result<DataSet>> + Send {
        self.as_client().query_dataset(command)
    }

    /// Run a [`Command`] for its row count rather than its rows.
    fn execute_command(&mut self, command: &Command) -> impl Future<Output = Result<u64>> + Send {
        self.as_client().execute_command(command)
    }
}

impl Executor for Client {
    fn transaction(&mut self) -> impl Future<Output = Result<Transaction<'_>>> + Send {
        Client::transaction(self)
    }
}

impl Executor for Transaction<'_> {
    fn transaction(&mut self) -> impl Future<Output = Result<Transaction<'_>>> + Send {
        Transaction::transaction(self)
    }
}

impl<T: Executor + ?Sized> Executor for &mut T {
    fn transaction(&mut self) -> impl Future<Output = Result<Transaction<'_>>> + Send {
        (**self).transaction()
    }
}
