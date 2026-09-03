//! Transactions and savepoints.

use crate::client::Client;
use crate::command::Command;
use crate::dataset::DataSet;
use crate::error::{Error, Result};
use crate::row::Row;
use crate::value::{FromSql, ToSql};

/// The isolation level a transaction runs at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum IsolationLevel {
    /// Whatever the server is configured to default to.
    #[default]
    Unspecified,
    /// `READ UNCOMMITTED` — dirty reads allowed.
    ReadUncommitted,
    /// `READ COMMITTED` — SQL Server's own default.
    ReadCommitted,
    /// `REPEATABLE READ`.
    RepeatableRead,
    /// `SERIALIZABLE`.
    Serializable,
    /// `SNAPSHOT` — requires the database to have snapshot isolation enabled.
    Snapshot,
}

impl From<IsolationLevel> for tiberius::IsolationLevel {
    fn from(level: IsolationLevel) -> Self {
        match level {
            IsolationLevel::Unspecified => tiberius::IsolationLevel::Unspecified,
            IsolationLevel::ReadUncommitted => tiberius::IsolationLevel::ReadUncommitted,
            IsolationLevel::ReadCommitted => tiberius::IsolationLevel::ReadCommitted,
            IsolationLevel::RepeatableRead => tiberius::IsolationLevel::RepeatableRead,
            IsolationLevel::Serializable => tiberius::IsolationLevel::Serializable,
            IsolationLevel::Snapshot => tiberius::IsolationLevel::Snapshot,
        }
    }
}

/// Whether this handle owns the transaction itself or just a savepoint in it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Scope {
    Root,
    Savepoint(String),
}

/// An in-progress transaction.
///
/// Created by [`Client::transaction`]. Every statement run through it is
/// scoped to the transaction. It borrows the client mutably, so the connection
/// cannot be used behind the transaction's back — which is honest, since TDS
/// carries one in-flight request per connection anyway.
///
/// Call [`commit`](Self::commit) to keep the work or [`rollback`](Self::rollback)
/// to discard it. **Dropping without doing either discards the work** — a
/// transaction never commits implicitly.
///
/// ```no_run
/// # use tdsql::Client;
/// # async fn f(client: &mut Client) -> tdsql::Result<()> {
/// let mut tx = client.transaction().await?;
///
/// tx.execute("INSERT INTO orders (id) VALUES (@P1)", &[&1i32]).await?;
/// tx.execute("UPDATE stock SET qty = qty - 1 WHERE id = @P1", &[&7i32]).await?;
///
/// tx.commit().await?;
/// # Ok(())
/// # }
/// ```
///
/// A `&mut Transaction` can be handed to helper functions, so the unit of work
/// can be assembled across several of them:
///
/// ```no_run
/// # use tdsql::{Client, Transaction};
/// async fn add_order(tx: &mut Transaction<'_>, id: i32) -> tdsql::Result<()> {
///     tx.execute("INSERT INTO orders (id) VALUES (@P1)", &[&id]).await?;
///     Ok(())
/// }
///
/// # async fn f(client: &mut Client) -> tdsql::Result<()> {
/// let mut tx = client.transaction().await?;
/// add_order(&mut tx, 1).await?;
/// add_order(&mut tx, 2).await?;
/// tx.commit().await?;
/// # Ok(())
/// # }
/// ```
///
/// # Rollback on drop
///
/// Dropping without committing rolls back. `Drop` cannot run `async` code, so
/// the rollback is *enqueued* on the connection task rather than awaited — the
/// send itself is synchronous and non-blocking. Requests are served in order,
/// so the rollback reaches the server before any later statement on this
/// connection. Use [`rollback`](Self::rollback) instead when you want to
/// observe an error from it.
#[derive(Debug)]
pub struct Transaction<'a> {
    client: &'a mut Client,
    scope: Scope,
    done: bool,
}

impl<'a> Transaction<'a> {
    pub(crate) async fn begin(client: &'a mut Client, level: IsolationLevel) -> Result<Self> {
        client.begin_transaction(level).await?;
        Ok(Self {
            client,
            scope: Scope::Root,
            done: false,
        })
    }

    /// Commit the work.
    ///
    /// For a savepoint this keeps the work and folds it into the surrounding
    /// transaction; SQL Server has no `RELEASE SAVEPOINT`, so nothing is sent.
    pub async fn commit(mut self) -> Result<()> {
        self.done = true;
        match &self.scope {
            Scope::Root => self.client.commit_transaction().await,
            Scope::Savepoint(_) => Ok(()),
        }
    }

    /// Discard the work.
    ///
    /// Equivalent to dropping the transaction, but reports any error instead of
    /// swallowing it, and takes effect immediately rather than before the next
    /// statement.
    pub async fn rollback(mut self) -> Result<()> {
        self.done = true;
        match &self.scope {
            Scope::Root => self.client.rollback_transaction().await,
            Scope::Savepoint(name) => {
                let name = name.clone();
                self.client.rollback_to_savepoint(&name).await
            }
        }
    }

    /// Open a nested transaction, backed by an automatically named savepoint.
    pub async fn transaction(&mut self) -> Result<Transaction<'_>> {
        let name = format!("_tdsql_sp{}", self.client.next_savepoint_id());
        self.savepoint(name).await
    }

    /// Open a nested transaction at a named savepoint.
    ///
    /// The name must be a plain SQL identifier — letters, digits and
    /// underscores, starting with a letter or underscore, at most 32
    /// characters, which is SQL Server's limit for savepoint names.
    pub async fn savepoint(&mut self, name: impl Into<String>) -> Result<Transaction<'_>> {
        let name = name.into();
        validate_savepoint_name(&name)?;
        self.client.save_transaction(&name).await?;
        Ok(Transaction {
            client: &mut *self.client,
            scope: Scope::Savepoint(name),
            done: false,
        })
    }

    /// Run a query and collect every row of the first result set.
    pub async fn query(&mut self, sql: &str, params: &[&dyn ToSql]) -> Result<Vec<Row>> {
        self.client.query(sql, params).await
    }

    /// Run a query expecting exactly one row.
    pub async fn query_one(&mut self, sql: &str, params: &[&dyn ToSql]) -> Result<Row> {
        self.client.query_one(sql, params).await
    }

    /// Run a query expecting at most one row.
    pub async fn query_opt(&mut self, sql: &str, params: &[&dyn ToSql]) -> Result<Option<Row>> {
        self.client.query_opt(sql, params).await
    }

    /// Run a query and read the first column of the first row.
    pub async fn query_scalar<T: FromSql>(
        &mut self,
        sql: &str,
        params: &[&dyn ToSql],
    ) -> Result<T> {
        self.client.query_scalar(sql, params).await
    }

    /// Run a statement and report how many rows it affected.
    pub async fn execute(&mut self, sql: &str, params: &[&dyn ToSql]) -> Result<u64> {
        self.client.execute(sql, params).await
    }

    /// Run a SQL batch verbatim and collect its rows.
    pub async fn batch(&mut self, sql: &str) -> Result<Vec<Row>> {
        self.client.batch(sql).await
    }

    /// Run a SQL batch verbatim and collect every result set.
    pub async fn batch_dataset(&mut self, sql: &str) -> Result<DataSet> {
        self.client.batch_dataset(sql).await
    }

    /// Run a [`Command`] and collect every result set it produces.
    pub async fn query_dataset(&mut self, command: &Command) -> Result<DataSet> {
        self.client.query_dataset(command).await
    }

    /// Run a [`Command`] for its row count rather than its rows.
    pub async fn execute_command(&mut self, command: &Command) -> Result<u64> {
        self.client.execute_command(command).await
    }
}

impl Drop for Transaction<'_> {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        // `Drop` cannot await, but it can *enqueue*: the channel send is
        // synchronous and non-blocking, and the connection task serves
        // requests in order, so the rollback is guaranteed to reach the server
        // before any later statement.
        match &self.scope {
            Scope::Root => self.client.enqueue_rollback(),
            Scope::Savepoint(name) => self.client.enqueue_rollback_to(name.clone()),
        }
    }
}

/// SQL Server savepoint names are identifiers, and they are interpolated into
/// `ROLLBACK TRANSACTION <name>`, so they must be checked rather than trusted.
fn validate_savepoint_name(name: &str) -> Result<()> {
    let valid = !name.is_empty()
        && name.len() <= 32
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');

    if valid {
        Ok(())
    } else {
        Err(Error::InvalidSavepointName(name.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_plain_identifiers() {
        assert!(validate_savepoint_name("sp1").is_ok());
        assert!(validate_savepoint_name("_tdsql_sp0").is_ok());
        assert!(validate_savepoint_name("Before_Risky_Bit").is_ok());
    }

    #[test]
    fn rejects_injection_and_malformed_names() {
        for bad in [
            "",
            "1sp",
            "sp name",
            "sp;DROP TABLE users",
            "sp'--",
            "sp-1",
            "spécial",
            &"x".repeat(33),
        ] {
            assert!(
                validate_savepoint_name(bad).is_err(),
                "should have rejected {bad:?}"
            );
        }
    }

    #[test]
    fn rejects_at_the_length_boundary() {
        assert!(validate_savepoint_name(&"x".repeat(32)).is_ok());
        assert!(validate_savepoint_name(&"x".repeat(33)).is_err());
    }

    #[test]
    fn maps_isolation_levels() {
        use tiberius::IsolationLevel as T;
        assert_eq!(T::from(IsolationLevel::Unspecified), T::Unspecified);
        assert_eq!(T::from(IsolationLevel::ReadCommitted), T::ReadCommitted);
        assert_eq!(T::from(IsolationLevel::Serializable), T::Serializable);
        assert_eq!(T::from(IsolationLevel::Snapshot), T::Snapshot);
    }

    #[test]
    fn defaults_to_unspecified() {
        assert_eq!(IsolationLevel::default(), IsolationLevel::Unspecified);
    }
}
