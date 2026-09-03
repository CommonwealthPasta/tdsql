//! The connection: connect once, run many statements.
//!
//! The connection lives in a background task that owns the driver client.
//! [`Client`] is a handle that sends it requests over a channel and awaits the
//! reply. That indirection buys two things: a request is always run to
//! completion even if the caller stops awaiting it, so a dropped future cannot
//! desync the protocol stream; and work can be *enqueued* without `await`,
//! which is what lets [`Transaction`](crate::Transaction) roll back from
//! `Drop`.

use std::borrow::Cow;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use futures::StreamExt;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

use crate::command::{Command, Prepared};
use crate::config::Config;
use crate::dataset::{DataSet, DataTable};
use crate::error::{Error, Result};
use crate::row::{Column, Row};
use crate::transaction::{IsolationLevel, Transaction};
use crate::value::{DataValue, FromSql, SqlType, ToSql};

type Driver = tiberius::Client<Compat<TcpStream>>;

// ---------------------------------------------------------------------------
// Binding: DataValue -> the driver's wire types.
// ---------------------------------------------------------------------------

impl tiberius::ToSql for DataValue {
    fn to_sql(&self) -> tiberius::ColumnData<'_> {
        use tiberius::ColumnData as C;
        match self {
            DataValue::TinyInt(v) => C::U8(Some(*v)),
            DataValue::SmallInt(v) => C::I16(Some(*v)),
            DataValue::Int(v) => C::I32(Some(*v)),
            DataValue::BigInt(v) => C::I64(Some(*v)),
            DataValue::Float(v) => C::F64(Some(*v)),
            DataValue::Decimal(v) => v.to_sql(),
            DataValue::Bool(v) => C::Bit(Some(*v)),
            DataValue::Text(v) => C::String(Some(Cow::Borrowed(v.as_str()))),
            DataValue::Binary(v) => C::Binary(Some(Cow::Borrowed(v.as_slice()))),
            DataValue::Guid(v) => C::Guid(Some(*v)),
            DataValue::Date(v) => v.to_sql(),
            DataValue::Time(v) => v.to_sql(),
            DataValue::DateTime(v) => v.to_sql(),
            DataValue::DateTimeOffset(v) => v.to_sql(),
            // An untyped NULL. SQL Server coerces this for most targets; bind a
            // typed `Option<T>` when the column needs a specific type.
            DataValue::Null => C::I32(None),
        }
    }
}

impl<'a> tiberius::IntoSql<'a> for DataValue {
    fn into_sql(self) -> tiberius::ColumnData<'a> {
        use tiberius::ColumnData as C;
        match self {
            DataValue::Text(v) => C::String(Some(Cow::Owned(v))),
            DataValue::Binary(v) => C::Binary(Some(Cow::Owned(v))),
            // Every other variant borrows nothing.
            other => match tiberius::ToSql::to_sql(&other) {
                C::U8(v) => C::U8(v),
                C::I16(v) => C::I16(v),
                C::I32(v) => C::I32(v),
                C::I64(v) => C::I64(v),
                C::F32(v) => C::F32(v),
                C::F64(v) => C::F64(v),
                C::Bit(v) => C::Bit(v),
                C::Guid(v) => C::Guid(v),
                C::Numeric(v) => C::Numeric(v),
                C::DateTime(v) => C::DateTime(v),
                C::SmallDateTime(v) => C::SmallDateTime(v),
                C::Time(v) => C::Time(v),
                C::Date(v) => C::Date(v),
                C::DateTime2(v) => C::DateTime2(v),
                C::DateTimeOffset(v) => C::DateTimeOffset(v),
                C::String(v) => C::String(v.map(|s| Cow::Owned(s.into_owned()))),
                C::Binary(v) => C::Binary(v.map(|b| Cow::Owned(b.into_owned()))),
                C::Xml(v) => C::Xml(v.map(|x| Cow::Owned(x.into_owned()))),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Reading: the driver's wire types -> DataValue.
// ---------------------------------------------------------------------------

/// Convert one column value, naming `column` in any error.
///
/// This is the crate's single mapping path: every query goes through it, so
/// the row-returning and scalar-returning paths can never drift.
pub(crate) fn map_column_data(cd: tiberius::ColumnData<'_>, column: &str) -> Result<DataValue> {
    use tiberius::ColumnData as C;

    // A temporal value the server sent but `chrono` cannot represent.
    fn temporal(column: &str, target: &'static str) -> impl Fn(tiberius::error::Error) -> Error {
        let column = column.to_string();
        move |_| Error::Conversion {
            column: column.clone(),
            actual: "temporal",
            target,
        }
    }

    Ok(match cd {
        C::U8(v) => v.map(DataValue::TinyInt).unwrap_or_default(),
        C::I16(v) => v.map(DataValue::SmallInt).unwrap_or_default(),
        C::I32(v) => v.map(DataValue::Int).unwrap_or_default(),
        C::I64(v) => v.map(DataValue::BigInt).unwrap_or_default(),
        C::F32(v) => v.map(|f| DataValue::Float(f as f64)).unwrap_or_default(),
        C::F64(v) => v.map(DataValue::Float).unwrap_or_default(),
        C::Bit(v) => v.map(DataValue::Bool).unwrap_or_default(),
        C::String(v) => v
            .map(|s| DataValue::Text(s.into_owned()))
            .unwrap_or_default(),
        C::Guid(v) => v.map(DataValue::Guid).unwrap_or_default(),
        C::Binary(v) => v
            .map(|b| DataValue::Binary(b.into_owned()))
            .unwrap_or_default(),
        C::Xml(v) => v
            .map(|x| DataValue::Text(x.as_ref().to_string()))
            .unwrap_or_default(),

        // A numeric out of `Decimal`'s range used to become NULL silently.
        C::Numeric(v) => match v {
            None => DataValue::Null,
            Some(n) => rust_decimal::Decimal::try_from_i128_with_scale(n.value(), n.scale() as u32)
                .map(DataValue::Decimal)
                .map_err(|_| Error::Decimal {
                    value: n.value(),
                    scale: n.scale(),
                })?,
        },

        // These used to `.unwrap()`, panicking the caller's task on bad data.
        C::DateTime(v) => {
            <chrono::NaiveDateTime as tiberius::FromSqlOwned>::from_sql_owned(C::DateTime(v))
                .map_err(temporal(column, "NaiveDateTime"))?
                .map(DataValue::DateTime)
                .unwrap_or_default()
        }

        C::SmallDateTime(v) => {
            <chrono::NaiveDateTime as tiberius::FromSqlOwned>::from_sql_owned(C::SmallDateTime(v))
                .map_err(temporal(column, "NaiveDateTime"))?
                .map(DataValue::DateTime)
                .unwrap_or_default()
        }

        C::DateTime2(v) => {
            <chrono::NaiveDateTime as tiberius::FromSqlOwned>::from_sql_owned(C::DateTime2(v))
                .map_err(temporal(column, "NaiveDateTime"))?
                .map(DataValue::DateTime)
                .unwrap_or_default()
        }

        C::Time(v) => <chrono::NaiveTime as tiberius::FromSqlOwned>::from_sql_owned(C::Time(v))
            .map_err(temporal(column, "NaiveTime"))?
            .map(DataValue::Time)
            .unwrap_or_default(),

        C::Date(v) => <chrono::NaiveDate as tiberius::FromSqlOwned>::from_sql_owned(C::Date(v))
            .map_err(temporal(column, "NaiveDate"))?
            .map(DataValue::Date)
            .unwrap_or_default(),

        C::DateTimeOffset(v) => {
            <chrono::DateTime<chrono::FixedOffset> as tiberius::FromSqlOwned>::from_sql_owned(
                C::DateTimeOffset(v),
            )
            .map_err(temporal(column, "DateTime<FixedOffset>"))?
            .map(DataValue::DateTimeOffset)
            .unwrap_or_default()
        }
    })
}

fn columns_of(meta: &tiberius::ResultMetadata) -> Arc<[Column]> {
    meta.columns()
        .iter()
        .map(|c| Column::new(c.name(), SqlType::from(c.column_type())))
        .collect()
}

fn row_of(row: tiberius::Row, columns: &Arc<[Column]>) -> Result<Row> {
    let mut values = Vec::with_capacity(columns.len());
    for (i, cd) in row.into_iter().enumerate() {
        let name = columns.get(i).map(Column::name).unwrap_or("?");
        values.push(map_column_data(cd, name)?);
    }
    Ok(Row::new(Arc::clone(columns), values))
}

/// Collect a query stream into its result sets, in server order.
async fn collect_query_stream(mut stream: tiberius::QueryStream<'_>) -> Result<DataSet> {
    let mut dataset = DataSet::new();
    let mut current: Option<(DataTable, Arc<[Column]>)> = None;

    while let Some(item) = stream.next().await {
        match item? {
            tiberius::QueryItem::Metadata(meta) => {
                if let Some((table, _)) = current.take() {
                    dataset.push(table);
                }
                let columns = columns_of(&meta);
                let name = format!("table{}", meta.result_index());
                current = Some((DataTable::new(name, Arc::clone(&columns)), columns));
            }
            tiberius::QueryItem::Row(row) => {
                let (table, columns) = current.get_or_insert_with(|| {
                    let columns: Arc<[Column]> = Arc::from(Vec::new());
                    (DataTable::new("table0", Arc::clone(&columns)), columns)
                });
                table.push(row_of(row, columns)?);
            }
        }
    }

    if let Some((table, _)) = current.take() {
        dataset.push(table);
    }
    Ok(dataset)
}

fn as_tiberius_refs(values: &[DataValue]) -> Vec<&dyn tiberius::ToSql> {
    values.iter().map(|v| v as &dyn tiberius::ToSql).collect()
}

pub(crate) fn to_values(params: &[&dyn ToSql]) -> Vec<DataValue> {
    params.iter().map(|p| p.to_value()).collect()
}

// ---------------------------------------------------------------------------
// The connection task
// ---------------------------------------------------------------------------

type Reply<T> = oneshot::Sender<Result<T>>;

/// One unit of work for the connection task.
enum Request {
    Query {
        sql: String,
        params: Vec<DataValue>,
        reply: Reply<DataSet>,
    },
    Execute {
        sql: String,
        params: Vec<DataValue>,
        reply: Reply<u64>,
    },
    Batch {
        sql: String,
        reply: Reply<DataSet>,
    },
    ProcQuery {
        name: String,
        params: Vec<(String, DataValue)>,
        reply: Reply<DataSet>,
    },
    ProcExecute {
        name: String,
        params: Vec<(String, DataValue)>,
        reply: Reply<u64>,
    },
    Begin {
        level: IsolationLevel,
        reply: Reply<()>,
    },
    Commit {
        reply: Reply<()>,
    },
    Savepoint {
        name: String,
        reply: Reply<()>,
    },
    /// `reply` is `None` when enqueued from `Transaction::drop`, which has no
    /// caller left to report to.
    Rollback {
        reply: Option<Reply<()>>,
    },
    RollbackTo {
        name: String,
        reply: Option<Reply<()>>,
    },
}

/// Own the driver and serve requests until every handle is gone.
///
/// Requests are served strictly in order, which is also what TDS requires:
/// there is no MARS, so a connection carries one in-flight request at a time.
async fn connection_task(mut driver: Driver, mut rx: mpsc::UnboundedReceiver<Request>) {
    while let Some(request) = rx.recv().await {
        match request {
            Request::Query { sql, params, reply } => {
                let result = run_query(&mut driver, &sql, &params).await;
                let _ = reply.send(result);
            }
            Request::Execute { sql, params, reply } => {
                let refs = as_tiberius_refs(&params);
                let result = driver
                    .execute(&sql, &refs)
                    .await
                    .map(|r| r.total())
                    .map_err(Error::from);
                drop(refs);
                let _ = reply.send(result);
            }
            Request::Batch { sql, reply } => {
                let result = match driver.simple_query(&sql).await {
                    Ok(stream) => collect_query_stream(stream).await,
                    Err(e) => Err(Error::from(e)),
                };
                let _ = reply.send(result);
            }
            Request::ProcQuery {
                name,
                params,
                reply,
            } => {
                let result = run_proc_query(&mut driver, name, params).await;
                let _ = reply.send(result);
            }
            Request::ProcExecute {
                name,
                params,
                reply,
            } => {
                let result = run_proc_execute(&mut driver, name, params).await;
                let _ = reply.send(result);
            }
            Request::Begin { level, reply } => {
                let result = driver
                    .begin_transaction_with_isolation(level.into())
                    .await
                    .map_err(Error::from);
                let _ = reply.send(result);
            }
            Request::Commit { reply } => {
                let result = driver.commit_transaction().await.map_err(Error::from);
                let _ = reply.send(result);
            }
            Request::Savepoint { name, reply } => {
                // Not `driver.save_transaction`: tiberius-ng 0.13.1 encodes the
                // TM_SAVE_XACT name length as a count of UTF-16 code units
                // where the server reads a byte count, so any non-empty name is
                // rejected ("odd length N" / "TM request is longer than
                // expected"). T-SQL is the documented alternative and is what
                // the matching rollback already uses. The name is validated as
                // an identifier before it reaches here.
                // Bracket-quoted so reserved words (`inner`, `key`, ...) work.
                // Validation guarantees the name cannot contain `]`.
                let sql = format!("SAVE TRANSACTION [{name}]");
                let result = match driver.simple_query(&sql).await {
                    Ok(stream) => collect_query_stream(stream).await.map(|_| ()),
                    Err(e) => Err(Error::from(e)),
                };
                let _ = reply.send(result);
            }
            Request::Rollback { reply } => {
                let result = driver.rollback_transaction().await.map_err(Error::from);
                if let Some(reply) = reply {
                    let _ = reply.send(result);
                }
            }
            Request::RollbackTo { name, reply } => {
                // TDS has no "rollback to savepoint" manager request; T-SQL is
                // the documented route. The name is validated as an identifier
                // before it ever reaches here.
                let sql = format!("ROLLBACK TRANSACTION [{name}]");
                let result = match driver.simple_query(&sql).await {
                    Ok(stream) => collect_query_stream(stream).await.map(|_| ()),
                    Err(e) => Err(Error::from(e)),
                };
                if let Some(reply) = reply {
                    let _ = reply.send(result);
                }
            }
        }
    }

    // Every handle is gone. Close politely; an error here has nobody to tell.
    let _ = driver.close().await;
}

async fn run_query(driver: &mut Driver, sql: &str, params: &[DataValue]) -> Result<DataSet> {
    let refs = as_tiberius_refs(params);
    let stream = driver.query(sql, &refs).await?;
    collect_query_stream(stream).await
}

fn build_proc<'a>(name: String, params: Vec<(String, DataValue)>) -> tiberius::Command<'a> {
    let mut cmd = tiberius::Command::new(name);
    for (pname, value) in params {
        cmd.bind_param(pname, value);
    }
    cmd
}

async fn run_proc_query(
    driver: &mut Driver,
    name: String,
    params: Vec<(String, DataValue)>,
) -> Result<DataSet> {
    let mut stream = build_proc(name, params).exec(driver).await?;

    let mut dataset = DataSet::new();
    let mut current: Option<(DataTable, Arc<[Column]>)> = None;

    while let Some(item) = stream.next().await {
        match item? {
            tiberius::CommandItem::Metadata(meta) => {
                if let Some((table, _)) = current.take() {
                    dataset.push(table);
                }
                let columns = columns_of(&meta);
                let tname = format!("table{}", meta.result_index());
                current = Some((DataTable::new(tname, Arc::clone(&columns)), columns));
            }
            tiberius::CommandItem::Row(row) => {
                let (table, columns) = current.get_or_insert_with(|| {
                    let columns: Arc<[Column]> = Arc::from(Vec::new());
                    (DataTable::new("table0", Arc::clone(&columns)), columns)
                });
                table.push(row_of(row, columns)?);
            }
            _ => {}
        }
    }

    if let Some((table, _)) = current.take() {
        dataset.push(table);
    }
    Ok(dataset)
}

async fn run_proc_execute(
    driver: &mut Driver,
    name: String,
    params: Vec<(String, DataValue)>,
) -> Result<u64> {
    let mut stream = build_proc(name, params).exec(driver).await?;
    let mut total = 0u64;
    while let Some(item) = stream.next().await {
        if let tiberius::CommandItem::RowsAffected(n) = item? {
            total += n;
        }
    }
    Ok(total)
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// A connection to SQL Server.
///
/// Connect once and reuse it; every statement travels over the same
/// connection.
///
/// ```no_run
/// use tdsql::{Client, Config};
///
/// # async fn f() -> tdsql::Result<()> {
/// let mut client = Client::connect(&Config::new()
///     .host("localhost")
///     .database("master")
///     .auth("sa", "YourStrong!Passw0rd")
///     .trust_cert()).await?;
///
/// let rows = client.query("SELECT id, name FROM users WHERE id > @P1", &[&10i32]).await?;
/// for row in &rows {
///     let id: i32 = row.get("id");
///     let name: String = row.get("name");
///     println!("{id}: {name}");
/// }
/// # Ok(())
/// # }
/// ```
pub struct Client {
    tx: mpsc::UnboundedSender<Request>,
    savepoints: AtomicU64,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("open", &!self.tx.is_closed())
            .finish()
    }
}

impl Client {
    /// Connect using this configuration.
    pub async fn connect(config: &Config) -> Result<Self> {
        let cfg = config.to_tiberius()?;
        let addr = cfg.get_addr();
        let tcp = TcpStream::connect(&addr).await?;
        tcp.set_nodelay(true)?;
        let driver = tiberius::Client::connect(cfg, tcp.compat_write())
            .await
            .map_err(|e| Error::Connect {
                server: addr,
                source: Box::new(e),
            })?;

        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(connection_task(driver, rx));

        Ok(Self {
            tx,
            savepoints: AtomicU64::new(0),
        })
    }

    /// Connect using an ADO.NET-style connection string.
    pub async fn connect_str(connection_string: &str) -> Result<Self> {
        Self::connect(&Config::from_ado_string(connection_string)).await
    }

    /// Send a request and await its reply.
    async fn call<T>(&self, build: impl FnOnce(Reply<T>) -> Request) -> Result<T> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(build(reply_tx))
            .map_err(|_| Error::ConnectionClosed)?;
        reply_rx.await.map_err(|_| Error::ConnectionClosed)?
    }

    /// Enqueue work without waiting for it. Used by `Transaction::drop`, which
    /// cannot await. A closed channel means the connection is already gone, in
    /// which case the server has rolled the transaction back for us.
    fn enqueue(&self, request: Request) {
        let _ = self.tx.send(request);
    }

    /// Run a query and collect every row of the first result set.
    ///
    /// Parameters are positional: `@P1` is `params[0]`.
    pub async fn query(&mut self, sql: &str, params: &[&dyn ToSql]) -> Result<Vec<Row>> {
        Ok(self
            .query_dataset_values(sql, to_values(params))
            .await?
            .into_rows())
    }

    /// Run a query expecting exactly one row.
    ///
    /// Fails with [`Error::UnexpectedRowCount`] for any other count.
    pub async fn query_one(&mut self, sql: &str, params: &[&dyn ToSql]) -> Result<Row> {
        let mut rows = self.query(sql, params).await?;
        match rows.len() {
            1 => Ok(rows.pop().expect("length checked")),
            found => Err(Error::UnexpectedRowCount { found }),
        }
    }

    /// Run a query expecting at most one row.
    ///
    /// Fails with [`Error::UnexpectedRowCount`] if more than one comes back.
    pub async fn query_opt(&mut self, sql: &str, params: &[&dyn ToSql]) -> Result<Option<Row>> {
        let mut rows = self.query(sql, params).await?;
        match rows.len() {
            0 => Ok(None),
            1 => Ok(Some(rows.pop().expect("length checked"))),
            found => Err(Error::UnexpectedRowCount { found }),
        }
    }

    /// Run a query and read the first column of the first row.
    ///
    /// Use `Option<T>` as `T` to accept a `NULL`. Fails with
    /// [`Error::UnexpectedRowCount`] if the query returns no rows.
    pub async fn query_scalar<T: FromSql>(
        &mut self,
        sql: &str,
        params: &[&dyn ToSql],
    ) -> Result<T> {
        let row = self.query_one(sql, params).await?;
        if row.is_empty() {
            return Err(Error::ColumnIndexOutOfRange { index: 0, len: 0 });
        }
        row.try_get(0usize)
    }

    /// Run a statement that returns no rows, and report how many rows it
    /// affected.
    ///
    /// If the batch holds several statements, the counts are summed.
    pub async fn execute(&mut self, sql: &str, params: &[&dyn ToSql]) -> Result<u64> {
        let (sql, params) = (sql.to_string(), to_values(params));
        self.call(|reply| Request::Execute { sql, params, reply })
            .await
    }

    /// Run a SQL batch verbatim, with no parameters, and collect its rows.
    ///
    /// Parameterised statements are sent as an RPC, and some statements —
    /// `CREATE PROCEDURE`, `CREATE VIEW`, `CREATE TRIGGER` — must be the first
    /// statement of their own batch, so they cannot go that route. Use this for
    /// DDL, and [`execute`](Self::execute) or [`query`](Self::query) for
    /// everything else.
    ///
    /// Because the SQL is sent verbatim, never interpolate untrusted input into
    /// it; use parameters instead.
    ///
    /// ```no_run
    /// # use tdsql::Client;
    /// # async fn f(client: &mut Client) -> tdsql::Result<()> {
    /// client
    ///     .batch("CREATE PROCEDURE dbo.sp_demo AS BEGIN SELECT 1; END")
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn batch(&mut self, sql: &str) -> Result<Vec<Row>> {
        Ok(self.batch_dataset(sql).await?.into_rows())
    }

    /// Run a SQL batch verbatim and collect every result set it produces.
    pub async fn batch_dataset(&mut self, sql: &str) -> Result<DataSet> {
        let sql = sql.to_string();
        self.call(|reply| Request::Batch { sql, reply }).await
    }

    /// Run a [`Command`] and collect every result set it produces.
    ///
    /// This is the path for stored procedures and for batches that return more
    /// than one result set.
    pub async fn query_dataset(&mut self, command: &Command) -> Result<DataSet> {
        match command.prepare() {
            Prepared::Text { sql, params } => self.query_dataset_values(&sql, params).await,
            Prepared::Proc { name, params } => {
                self.call(|reply| Request::ProcQuery {
                    name,
                    params,
                    reply,
                })
                .await
            }
        }
    }

    /// Run a [`Command`] for its row count rather than its rows.
    pub async fn execute_command(&mut self, command: &Command) -> Result<u64> {
        match command.prepare() {
            Prepared::Text { sql, params } => {
                self.call(|reply| Request::Execute { sql, params, reply })
                    .await
            }
            Prepared::Proc { name, params } => {
                self.call(|reply| Request::ProcExecute {
                    name,
                    params,
                    reply,
                })
                .await
            }
        }
    }

    async fn query_dataset_values(&self, sql: &str, params: Vec<DataValue>) -> Result<DataSet> {
        let sql = sql.to_string();
        self.call(|reply| Request::Query { sql, params, reply })
            .await
    }

    /// Begin a transaction at the server's default isolation level.
    ///
    /// The returned [`Transaction`] borrows this client, so nothing else can
    /// use the connection until it is committed, rolled back, or dropped.
    ///
    /// ```no_run
    /// # use tdsql::Client;
    /// # async fn f(client: &mut Client) -> tdsql::Result<()> {
    /// let mut tx = client.transaction().await?;
    /// tx.execute("INSERT INTO orders (id) VALUES (@P1)", &[&1i32]).await?;
    /// tx.commit().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn transaction(&mut self) -> Result<Transaction<'_>> {
        Transaction::begin(self, IsolationLevel::Unspecified).await
    }

    /// Begin a transaction at an explicit isolation level.
    pub async fn transaction_with_isolation(
        &mut self,
        level: IsolationLevel,
    ) -> Result<Transaction<'_>> {
        Transaction::begin(self, level).await
    }

    /// Close the connection, sending a graceful logout.
    pub async fn close(self) -> Result<()> {
        // Dropping the last sender ends the task, which closes the driver.
        drop(self);
        Ok(())
    }

    // -- used by Transaction ------------------------------------------------

    pub(crate) async fn begin_transaction(&mut self, level: IsolationLevel) -> Result<()> {
        self.call(|reply| Request::Begin { level, reply }).await
    }

    pub(crate) async fn commit_transaction(&mut self) -> Result<()> {
        self.call(|reply| Request::Commit { reply }).await
    }

    pub(crate) async fn rollback_transaction(&mut self) -> Result<()> {
        self.call(|reply| Request::Rollback { reply: Some(reply) })
            .await
    }

    pub(crate) async fn save_transaction(&mut self, name: &str) -> Result<()> {
        let name = name.to_string();
        self.call(|reply| Request::Savepoint { name, reply }).await
    }

    pub(crate) async fn rollback_to_savepoint(&mut self, name: &str) -> Result<()> {
        let name = name.to_string();
        self.call(|reply| Request::RollbackTo {
            name,
            reply: Some(reply),
        })
        .await
    }

    /// Queue a rollback without awaiting it. Called from `Transaction::drop`.
    pub(crate) fn enqueue_rollback(&self) {
        self.enqueue(Request::Rollback { reply: None });
    }

    /// Queue a rollback to a savepoint without awaiting it.
    pub(crate) fn enqueue_rollback_to(&self, name: String) {
        self.enqueue(Request::RollbackTo { name, reply: None });
    }

    pub(crate) fn next_savepoint_id(&self) -> u64 {
        self.savepoints.fetch_add(1, Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests;
