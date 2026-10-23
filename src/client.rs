//! The connection: connect once, run many statements.
//!
//! The connection lives in a background task that owns the driver client.
//! [`Client`] is a handle that sends it requests over a channel and awaits the
//! reply. That indirection buys two things: the task, not the caller's future,
//! reads every response to its end -- and once the caller has gone, the
//! server's acknowledgement of a cancel -- so a dropped future cannot desync
//! the protocol stream; and work can be *enqueued* without `await`, which is
//! what lets [`Transaction`](crate::Transaction) roll back from `Drop`.

use std::borrow::Cow;
use std::pin::pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures::StreamExt;
use futures::future::{self, Either};
use tds_driver::SqlBrowser;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

use crate::command::{Command, Prepared};
use crate::config::Config;
use crate::dataset::{DataSet, DataTable};
use crate::error::{Error, Result, StatementKind};
use crate::row::{Column, Row};
use crate::stream::{RowStream, Streamed};
use crate::transaction::{IsolationLevel, Transaction};
use crate::value::{DataValue, FromSql, SqlType, ToSql};

type Driver = tds_driver::Client<Compat<TcpStream>>;

// ---------------------------------------------------------------------------
// Binding: DataValue -> the driver's wire types.
// ---------------------------------------------------------------------------

impl tds_driver::ToSql for DataValue {
    fn to_sql(&self) -> tds_driver::ColumnData<'_> {
        use tds_driver::ColumnData as C;
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
            // With no type to go on, `nvarchar`: SQL Server converts it
            // implicitly to every column type but the binary ones.
            DataValue::Null => C::String(None),
            DataValue::TypedNull(ty) => typed_null(*ty),
        }
    }
}

/// A `NULL` parameter declared as `ty`.
fn typed_null(ty: SqlType) -> tds_driver::ColumnData<'static> {
    use tds_driver::ColumnData as C;
    match ty {
        SqlType::Bit | SqlType::BitN => C::Bit(None),
        SqlType::TinyInt => C::U8(None),
        SqlType::SmallInt => C::I16(None),
        SqlType::Int => C::I32(None),
        // `IntN` is any nullable integer, and `bigint` holds every width.
        SqlType::BigInt | SqlType::IntN => C::I64(None),
        SqlType::Real => C::F32(None),
        SqlType::Float | SqlType::FloatN => C::F64(None),
        SqlType::Decimal | SqlType::Numeric | SqlType::Money | SqlType::SmallMoney => {
            C::Numeric(None)
        }
        SqlType::Guid => C::Guid(None),
        SqlType::Date => C::Date(None),
        SqlType::Time => C::Time(None),
        SqlType::DateTime | SqlType::DateTimeN => C::DateTime(None),
        SqlType::SmallDateTime => C::SmallDateTime(None),
        SqlType::DateTime2 => C::DateTime2(None),
        SqlType::DateTimeOffset => C::DateTimeOffset(None),
        SqlType::Binary | SqlType::VarBinary | SqlType::Image => C::Binary(None),
        SqlType::Xml => C::Xml(None),
        SqlType::Char
        | SqlType::VarChar
        | SqlType::NChar
        | SqlType::NVarChar
        | SqlType::Text
        | SqlType::NText => C::String(None),
        // No parameter type of their own.
        SqlType::Null | SqlType::Udt | SqlType::Variant => {
            tds_driver::ToSql::to_sql(&DataValue::Null)
        }
    }
}

impl<'a> tds_driver::IntoSql<'a> for DataValue {
    fn into_sql(self) -> tds_driver::ColumnData<'a> {
        use tds_driver::ColumnData as C;
        match self {
            DataValue::Text(v) => C::String(Some(Cow::Owned(v))),
            DataValue::Binary(v) => C::Binary(Some(Cow::Owned(v))),
            // Every other variant borrows nothing.
            other => match tds_driver::ToSql::to_sql(&other) {
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

/// A server numeric as a `Decimal`.
///
/// `Decimal` holds 28 digits at a scale of at most 28, so a wide `decimal(38, 30)`
/// column can carry a value that fits only once its trailing zeros go: `0.5`
/// at scale 30 is `5` at scale 1. Those are dropped until it fits, as the
/// driver and SqlClient do; a value that still does not fit is an error.
fn to_decimal(n: tds_driver::numeric::Numeric) -> Result<rust_decimal::Decimal> {
    let (mut value, mut scale) = (n.value(), n.scale() as u32);
    loop {
        if let Ok(d) = rust_decimal::Decimal::try_from_i128_with_scale(value, scale) {
            return Ok(d);
        }
        if scale == 0 || value % 10 != 0 {
            return Err(Error::Decimal {
                value: n.value(),
                scale: n.scale(),
            });
        }
        value /= 10;
        scale -= 1;
    }
}

/// Convert one column value, naming `column` in any error.
///
/// This is the crate's single mapping path: every query goes through it, so
/// the row-returning and scalar-returning paths can never drift.
pub(crate) fn map_column_data(cd: tds_driver::ColumnData<'_>, column: &str) -> Result<DataValue> {
    use tds_driver::ColumnData as C;

    // A temporal value the server sent but `chrono` cannot represent.
    fn temporal(column: &str, target: &'static str) -> impl Fn(tds_driver::error::Error) -> Error {
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
            Some(n) => DataValue::Decimal(to_decimal(n)?),
        },

        // These used to `.unwrap()`, panicking the caller's task on bad data.
        C::DateTime(v) => {
            <chrono::NaiveDateTime as tds_driver::FromSqlOwned>::from_sql_owned(C::DateTime(v))
                .map_err(temporal(column, "NaiveDateTime"))?
                .map(DataValue::DateTime)
                .unwrap_or_default()
        }

        C::SmallDateTime(v) => {
            <chrono::NaiveDateTime as tds_driver::FromSqlOwned>::from_sql_owned(C::SmallDateTime(v))
                .map_err(temporal(column, "NaiveDateTime"))?
                .map(DataValue::DateTime)
                .unwrap_or_default()
        }

        C::DateTime2(v) => {
            <chrono::NaiveDateTime as tds_driver::FromSqlOwned>::from_sql_owned(C::DateTime2(v))
                .map_err(temporal(column, "NaiveDateTime"))?
                .map(DataValue::DateTime)
                .unwrap_or_default()
        }

        C::Time(v) => <chrono::NaiveTime as tds_driver::FromSqlOwned>::from_sql_owned(C::Time(v))
            .map_err(temporal(column, "NaiveTime"))?
            .map(DataValue::Time)
            .unwrap_or_default(),

        C::Date(v) => <chrono::NaiveDate as tds_driver::FromSqlOwned>::from_sql_owned(C::Date(v))
            .map_err(temporal(column, "NaiveDate"))?
            .map(DataValue::Date)
            .unwrap_or_default(),

        C::DateTimeOffset(v) => {
            <chrono::DateTime<chrono::FixedOffset> as tds_driver::FromSqlOwned>::from_sql_owned(
                C::DateTimeOffset(v),
            )
            .map_err(temporal(column, "DateTime<FixedOffset>"))?
            .map(DataValue::DateTimeOffset)
            .unwrap_or_default()
        }
    })
}

fn columns_of(meta: &tds_driver::ResultMetadata) -> Arc<[Column]> {
    meta.columns()
        .iter()
        .map(|c| Column::new(c.name(), SqlType::from(c.column_type())))
        .collect()
}

fn row_of(row: tds_driver::Row, columns: &Arc<[Column]>) -> Result<Row> {
    let mut values = Vec::with_capacity(columns.len());
    for (i, cd) in row.into_iter().enumerate() {
        let name = columns.get(i).map(Column::name).unwrap_or("?");
        values.push(map_column_data(cd, name)?);
    }
    Ok(Row::new(Arc::clone(columns), values))
}

/// Collect a query stream into its result sets, in server order.
async fn collect_query_stream(mut stream: tds_driver::QueryStream<'_>) -> Result<DataSet> {
    let mut dataset = DataSet::new();
    let mut current: Option<(DataTable, Arc<[Column]>)> = None;
    // The first failure, held back until the response has been fully read so
    // the connection is clean for the next request.
    let mut failure: Option<Error> = None;

    while let Some(item) = stream.next().await {
        match item {
            Ok(tds_driver::QueryItem::Metadata(meta)) => {
                if let Some((table, _)) = current.take() {
                    dataset.push(table);
                }
                let columns = columns_of(&meta);
                let name = format!("table{}", meta.result_index());
                current = Some((DataTable::new(name, Arc::clone(&columns)), columns));
            }
            Ok(tds_driver::QueryItem::Row(row)) if failure.is_none() => {
                let (table, columns) = current.get_or_insert_with(|| {
                    let columns: Arc<[Column]> = Arc::from(Vec::new());
                    (DataTable::new("table0", Arc::clone(&columns)), columns)
                });
                match row_of(row, columns) {
                    Ok(row) => table.push(row),
                    Err(e) => failure = Some(e),
                }
            }
            Ok(tds_driver::QueryItem::Row(_)) => {}
            // The driver holds a server error back until it has read to the
            // end of the response, so nothing follows this.
            Err(e) => {
                failure.get_or_insert(e.into());
                break;
            }
        }
    }

    if let Some(e) = failure {
        return Err(e);
    }
    if let Some((table, _)) = current.take() {
        dataset.push(table);
    }
    Ok(dataset)
}

/// A `NaiveDateTime` as the legacy `datetime` type: days since 1900-01-01 and
/// 1/300 s ticks, rounded to the nearest tick as the server rounds a literal,
/// so `10:00:00.003` becomes tick 1 (`.0033333`) and matches a `datetime`
/// column holding it.
fn to_legacy_datetime(dt: chrono::NaiveDateTime) -> tds_driver::time::DateTime {
    use chrono::Timelike;

    let base = chrono::NaiveDate::from_ymd_opt(1900, 1, 1).expect("a valid date");
    let mut days = (dt.date() - base).num_days();
    let time = dt.time();
    let ns =
        u64::from(time.num_seconds_from_midnight()) * 1_000_000_000 + u64::from(time.nanosecond());
    let mut ticks = (ns * 3 + 5_000_000) / 10_000_000;
    // 23:59:59.9999999 rounds up to the next day.
    if ticks >= 300 * 86_400 {
        days += 1;
        ticks -= 300 * 86_400;
    }
    tds_driver::time::DateTime::new(days as i32, ticks as u32)
}

/// A `DataValue::DateTime` sent as `datetime` rather than `datetime2`.
struct LegacyDateTime(tds_driver::time::DateTime);

impl tds_driver::ToSql for LegacyDateTime {
    fn to_sql(&self) -> tds_driver::ColumnData<'_> {
        tds_driver::ColumnData::DateTime(Some(self.0))
    }
}

impl tds_driver::IntoSql<'static> for LegacyDateTime {
    fn into_sql(self) -> tds_driver::ColumnData<'static> {
        tds_driver::ColumnData::DateTime(Some(self.0))
    }
}

/// A parameter as it goes to the driver.
enum Bound<'a> {
    Plain(&'a DataValue),
    Legacy(LegacyDateTime),
}

impl tds_driver::ToSql for Bound<'_> {
    fn to_sql(&self) -> tds_driver::ColumnData<'_> {
        match self {
            Bound::Plain(v) => v.to_sql(),
            Bound::Legacy(v) => v.to_sql(),
        }
    }
}

/// The parameters as the connection sends them: with `legacy_datetime`, a
/// `DataValue::DateTime` goes as `datetime`; see
/// [`Config::datetime_parameter_type`].
fn bind(values: &[DataValue], legacy_datetime: bool) -> Vec<Bound<'_>> {
    values
        .iter()
        .map(|v| match v {
            DataValue::DateTime(dt) if legacy_datetime => {
                Bound::Legacy(LegacyDateTime(to_legacy_datetime(*dt)))
            }
            v => Bound::Plain(v),
        })
        .collect()
}

fn as_driver_refs<'a>(values: &'a [Bound<'a>]) -> Vec<&'a dyn tds_driver::ToSql> {
    values.iter().map(|v| v as &dyn tds_driver::ToSql).collect()
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
    /// `header` gets the columns, then `rows` gets the rows as they arrive.
    QueryStream {
        sql: String,
        params: Vec<DataValue>,
        header: Reply<Arc<[Column]>>,
        rows: mpsc::Sender<Result<Streamed>>,
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

impl Request {
    /// Whether the caller has stopped waiting for this request.
    ///
    /// Ending a transaction never counts as abandoned: [`Transaction`] relies
    /// on its commit and rollbacks reaching the server whatever became of the
    /// futures that sent them. A begin nobody waits for is different: no
    /// `Transaction` will exist to end it, so it must not start.
    fn abandoned(&self) -> bool {
        match self {
            Request::Query { reply, .. }
            | Request::Batch { reply, .. }
            | Request::ProcQuery { reply, .. } => reply.is_closed(),
            Request::Execute { reply, .. } | Request::ProcExecute { reply, .. } => {
                reply.is_closed()
            }
            Request::QueryStream { header, .. } => header.is_closed(),
            Request::Begin { reply, .. } => reply.is_closed(),
            Request::Commit { .. }
            | Request::Savepoint { .. }
            | Request::Rollback { .. }
            | Request::RollbackTo { .. } => false,
        }
    }

    /// Answer with `err` without sending anything to the server.
    fn fail(self, err: Error) {
        match self {
            Request::Query { reply, .. }
            | Request::Batch { reply, .. }
            | Request::ProcQuery { reply, .. } => {
                let _ = reply.send(Err(err));
            }
            Request::Execute { reply, .. } | Request::ProcExecute { reply, .. } => {
                let _ = reply.send(Err(err));
            }
            Request::QueryStream { header, .. } => {
                let _ = header.send(Err(err));
            }
            Request::Begin { reply, .. }
            | Request::Commit { reply }
            | Request::Savepoint { reply, .. } => {
                let _ = reply.send(Err(err));
            }
            Request::Rollback { reply } | Request::RollbackTo { reply, .. } => {
                if let Some(reply) = reply {
                    let _ = reply.send(Err(err));
                }
            }
        }
    }
}

/// The transaction a [`Transaction`] handle holds open on the connection.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TxState {
    None,
    Open,
    /// The server rolled it back on its own -- a deadlock, an error under
    /// `XACT_ABORT ON`, a `ROLLBACK` in the transaction's SQL -- but the
    /// handle is still around. Its statements must not run in autocommit.
    Aborted,
}

/// How long a cancelled request gets to finish what is left of its response
/// and have the cancel acknowledged. Past this the state of the connection is
/// unknown, so it is closed rather than reused.
const CANCEL_TIMEOUT: Duration = Duration::from_secs(30);

/// Run `work` for a caller who may stop waiting, which `gone` reports. Then the
/// server is asked to stop the request, and `work` reads what is left of the
/// response, which the cancel cuts short.
///
/// `work` is never dropped partway: the driver reads a token a field at a
/// time, so that could leave half a token behind.
///
/// `None` if the cancel failed or took longer than [`CANCEL_TIMEOUT`], in
/// which case the connection can no longer be trusted.
async fn cancellable<T>(
    driver: &mut Driver,
    gone: impl Future<Output = ()>,
    work: impl AsyncFnOnce(&mut Driver) -> T,
) -> Option<T> {
    let cancel = driver.cancel_handle();
    // Unconstrained, as a response arriving fast enough to use up the task's
    // budget every time would otherwise keep `gone` from ever resolving.
    let gone = pin!(tokio::task::unconstrained(gone));
    let deadline;
    let output = match future::select(pin!(work(&mut *driver)), gone).await {
        Either::Left((output, _)) => return Some(output),
        Either::Right(((), work)) => {
            cancel.cancel();
            deadline = tokio::time::Instant::now() + CANCEL_TIMEOUT;
            tokio::time::timeout_at(deadline, work).await.ok()?
        }
    };
    // The acknowledgement, unless the response already held it.
    matches!(
        tokio::time::timeout_at(deadline, driver.cancel_query()).await,
        Ok(Ok(()))
    )
    .then_some(output)
}

/// Serve a request that replies with the result of `work`; see
/// [`cancellable`].
///
/// `false` if the connection can no longer be trusted.
async fn serve<T>(
    driver: &mut Driver,
    mut reply: Reply<T>,
    work: impl AsyncFnOnce(&mut Driver) -> Result<T>,
) -> bool {
    match cancellable(driver, reply.closed(), work).await {
        Some(result) => {
            let _ = reply.send(result);
            true
        }
        None => false,
    }
}

/// Puts the connection back at READ COMMITTED once a transaction begun at
/// another level has ended; that level would otherwise carry over to every
/// later statement. `false` if the connection can't be trusted any more.
async fn restore_isolation(driver: &mut Driver, level_changed: &mut bool) -> bool {
    if !std::mem::take(level_changed) {
        return true;
    }
    match driver
        .simple_query("SET TRANSACTION ISOLATION LEVEL READ COMMITTED")
        .await
    {
        Ok(stream) => collect_query_stream(stream).await.is_ok(),
        Err(_) => false,
    }
}

/// Opens a connection to the server `cfg` names, following one redirect.
async fn open(mut cfg: tds_driver::Config) -> tds_driver::Result<Driver> {
    let mut redirected = false;
    loop {
        let tcp = TcpStream::connect_named(&cfg).await?;
        tcp.set_nodelay(true)?;
        match tds_driver::Client::connect(cfg.clone(), tcp.compat_write()).await {
            // ENVCHANGE routing (MS-TDS 2.2.7.9): log in again where it says.
            Err(tds_driver::error::Error::Routing { host, port }) if !redirected => {
                redirected = true;
                cfg.host(host);
                cfg.port(port);
            }
            connected => return connected,
        }
    }
}

/// Own the driver and serve requests until every handle is gone.
///
/// Requests are served strictly in order, which is also what TDS requires:
/// there is no MARS, so a connection carries one in-flight request at a time.
///
/// A request whose caller has gone is cancelled: skipped if it has not been
/// sent yet, or stopped on the server if it has. If the server does not
/// acknowledge a cancel, the task ends and takes the connection with it, so
/// every request still queued, and every later one, fails with
/// [`Error::ConnectionClosed`].
async fn connection_task(
    mut driver: Driver,
    mut rx: mpsc::UnboundedReceiver<Request>,
    legacy_datetime: bool,
) {
    // A transaction began at a level other than READ COMMITTED. The level
    // outlives it on the connection (MS-TDS 2.2.6.9).
    let mut level_changed = false;
    let mut tx = TxState::None;

    while let Some(request) = rx.recv().await {
        // Not sent yet, so cancelling it is as simple as not sending it.
        if request.abandoned() {
            continue;
        }
        if tx == TxState::Aborted {
            match request {
                // Nothing left to undo, so the rollback the handle asks for,
                // or its drop enqueues, is done already.
                Request::Rollback { reply } => {
                    tx = TxState::None;
                    if let Some(reply) = reply {
                        let _ = reply.send(Ok(()));
                    }
                    if !restore_isolation(&mut driver, &mut level_changed).await {
                        return;
                    }
                }
                Request::Commit { reply } => {
                    tx = TxState::None;
                    let _ = reply.send(Err(Error::TransactionAborted));
                    if !restore_isolation(&mut driver, &mut level_changed).await {
                        return;
                    }
                }
                other => other.fail(Error::TransactionAborted),
            }
            continue;
        }
        match request {
            Request::Query { sql, params, reply } => {
                let work = async |driver: &mut Driver| {
                    run_query(driver, &sql, &params, legacy_datetime)
                        .await
                        .map_err(|e| e.with_statement(StatementKind::Query, &sql, params.len()))
                };
                if !serve(&mut driver, reply, work).await {
                    return;
                }
            }
            Request::QueryStream {
                sql,
                params,
                header,
                rows,
            } => {
                let work = async |driver: &mut Driver| {
                    stream_query(driver, &sql, &params, legacy_datetime, header, &rows).await
                };
                if cancellable(&mut driver, rows.closed(), work)
                    .await
                    .is_none()
                {
                    return;
                }
            }
            Request::Execute { sql, params, reply } => {
                let work = async |driver: &mut Driver| {
                    // A plain batch without parameters, as `open_query` sends.
                    let result = if params.is_empty() {
                        driver.simple_execute(&sql).await
                    } else {
                        let bound = bind(&params, legacy_datetime);
                        let refs = as_driver_refs(&bound);
                        driver.execute(&sql, &refs).await
                    };
                    result.map(|r| r.total()).map_err(|e| {
                        Error::from(e).with_statement(StatementKind::Query, &sql, params.len())
                    })
                };
                if !serve(&mut driver, reply, work).await {
                    return;
                }
            }
            Request::Batch { sql, reply } => {
                let work = async |driver: &mut Driver| {
                    match driver.simple_query(&sql).await {
                        Ok(stream) => collect_query_stream(stream).await,
                        Err(e) => Err(Error::from(e)),
                    }
                    .map_err(|e| e.with_statement(StatementKind::Batch, &sql, 0))
                };
                if !serve(&mut driver, reply, work).await {
                    return;
                }
            }
            Request::ProcQuery {
                name,
                params,
                reply,
            } => {
                let count = params.len();
                let work = async |driver: &mut Driver| {
                    run_proc_query(driver, &name, params, legacy_datetime)
                        .await
                        .map_err(|e| e.with_statement(StatementKind::StoredProcedure, &name, count))
                };
                if !serve(&mut driver, reply, work).await {
                    return;
                }
            }
            Request::ProcExecute {
                name,
                params,
                reply,
            } => {
                let count = params.len();
                let work = async |driver: &mut Driver| {
                    run_proc_execute(driver, &name, params, legacy_datetime)
                        .await
                        .map_err(|e| e.with_statement(StatementKind::StoredProcedure, &name, count))
                };
                if !serve(&mut driver, reply, work).await {
                    return;
                }
            }
            Request::Begin { level, reply } => {
                let result = driver
                    .begin_transaction_with_isolation(level.into())
                    .await
                    .map_err(Error::from)
                    .map_err(|e| {
                        e.with_statement(StatementKind::TransactionControl, "BEGIN TRANSACTION", 0)
                    });
                let began = result.is_ok();
                if began
                    && !matches!(
                        level,
                        IsolationLevel::ReadCommitted | IsolationLevel::Unspecified
                    )
                {
                    level_changed = true;
                }
                match reply.send(result) {
                    Ok(()) if began => tx = TxState::Open,
                    // The caller gave up while the begin was in flight, so no
                    // `Transaction` exists to end it. Left open, it would
                    // swallow every later statement and nest the next
                    // transaction, whose commit would then commit nothing.
                    Err(_) if began => {
                        if driver.rollback_transaction().await.is_err() {
                            return;
                        }
                        if !restore_isolation(&mut driver, &mut level_changed).await {
                            return;
                        }
                    }
                    _ => {}
                }
            }
            Request::Commit { reply } => {
                tx = TxState::None;
                let result = driver
                    .commit_transaction()
                    .await
                    .map_err(Error::from)
                    .map_err(|e| {
                        e.with_statement(StatementKind::TransactionControl, "COMMIT TRANSACTION", 0)
                    });
                let _ = reply.send(result);
                if !restore_isolation(&mut driver, &mut level_changed).await {
                    return;
                }
            }
            Request::Savepoint { name, reply } => {
                // T-SQL, as the matching rollback uses, which the driver has no
                // call for. The name is validated as an identifier before it
                // reaches here.
                // Bracket-quoted so reserved words (`inner`, `key`, ...) work.
                // Validation guarantees the name cannot contain `]`.
                let sql = format!("SAVE TRANSACTION [{name}]");
                let result = match driver.simple_query(&sql).await {
                    Ok(stream) => collect_query_stream(stream).await.map(|_| ()),
                    Err(e) => Err(Error::from(e)),
                };
                let _ = reply.send(
                    result
                        .map_err(|e| e.with_statement(StatementKind::TransactionControl, &sql, 0)),
                );
            }
            Request::Rollback { reply } => {
                tx = TxState::None;
                let result = driver
                    .rollback_transaction()
                    .await
                    .map_err(Error::from)
                    .map_err(|e| {
                        e.with_statement(
                            StatementKind::TransactionControl,
                            "ROLLBACK TRANSACTION",
                            0,
                        )
                    });
                if let Some(reply) = reply {
                    let _ = reply.send(result);
                }
                if !restore_isolation(&mut driver, &mut level_changed).await {
                    return;
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
                }
                .map_err(|e| e.with_statement(StatementKind::TransactionControl, &sql, 0));
                if let Some(reply) = reply {
                    let _ = reply.send(result);
                }
            }
        }
        // The server ends a transaction by itself on a deadlock, on an error
        // under XACT_ABORT ON, or at a ROLLBACK in the transaction's own SQL,
        // and says so in the response the driver has just read.
        if tx == TxState::Open && !driver.in_transaction() {
            tx = TxState::Aborted;
        }
    }

    // Every handle is gone. Close politely; an error here has nobody to tell.
    let _ = driver.close().await;
}

/// Send `sql` and return its response stream.
///
/// With parameters it goes through `sp_executesql`. Without, it goes as a
/// plain batch, as SqlClient sends it: `sp_executesql` runs its text in a
/// scope of its own, so a `SET` option would be undone on return and a
/// `BEGIN TRAN` reported as error 266 after taking effect.
async fn open_query<'a>(
    driver: &'a mut Driver,
    sql: &str,
    params: &[DataValue],
    legacy_datetime: bool,
) -> tds_driver::Result<tds_driver::QueryStream<'a>> {
    if params.is_empty() {
        driver.simple_query(sql.to_string()).await
    } else {
        let bound = bind(params, legacy_datetime);
        let refs = as_driver_refs(&bound);
        driver.query(sql.to_string(), &refs).await
    }
}

async fn run_query(
    driver: &mut Driver,
    sql: &str,
    params: &[DataValue],
    legacy_datetime: bool,
) -> Result<DataSet> {
    let stream = open_query(driver, sql, params, legacy_datetime).await?;
    collect_query_stream(stream).await
}

/// How many rows the connection task may read ahead of a [`RowStream`]'s
/// consumer. Past this it stops reading, the server's send buffer fills, and
/// the query waits -- so a slow consumer slows the query down instead of
/// piling rows up in memory. Kept small because a row can be large.
const STREAM_BUFFER: usize = 64;

/// Run a query, handing the rows of its first result set to `rows` as they
/// arrive rather than collecting them.
///
/// `header` gets the result set's columns before any row is sent, or the error
/// if the statement failed before producing one. Everything after that goes to
/// `rows`, which always ends with [`Streamed::End`] or an error.
///
/// Later result sets are read and dropped, but an error in one still fails the
/// stream, just as it fails `query`.
async fn stream_query(
    driver: &mut Driver,
    sql: &str,
    params: &[DataValue],
    legacy_datetime: bool,
    header: Reply<Arc<[Column]>>,
    rows: &mpsc::Sender<Result<Streamed>>,
) {
    let mut header = Some(header);
    let mut columns: Arc<[Column]> = Arc::from(Vec::new());
    // False once the first result set is over or the consumer has gone.
    let mut forwarding = true;
    // The first failure, held back until the response has been fully read so
    // the connection is clean for the next request.
    let mut failure: Option<Error> = None;

    match open_query(driver, sql, params, legacy_datetime).await {
        Err(e) => failure = Some(e.into()),
        Ok(mut stream) => {
            while let Some(item) = stream.next().await {
                match item {
                    Ok(tds_driver::QueryItem::Metadata(meta)) => match header.take() {
                        Some(header) => {
                            columns = columns_of(&meta);
                            forwarding = header.send(Ok(Arc::clone(&columns))).is_ok();
                        }
                        None => forwarding = false,
                    },
                    Ok(tds_driver::QueryItem::Row(row)) if forwarding => {
                        match row_of(row, &columns) {
                            Ok(row) => {
                                forwarding = rows.send(Ok(Streamed::Row(row))).await.is_ok();
                            }
                            Err(e) => {
                                failure = Some(e);
                                forwarding = false;
                            }
                        }
                    }
                    Ok(tds_driver::QueryItem::Row(_)) => {}
                    // The driver holds a server error back until it has read
                    // to the end of the response, so nothing follows this.
                    Err(e) => {
                        failure.get_or_insert(e.into());
                        break;
                    }
                }
            }
        }
    }

    let last = match failure {
        Some(e) => Err(e.with_statement(StatementKind::Query, sql, params.len())),
        None => Ok(Streamed::End),
    };
    match header {
        // Still unsent: the statement failed, or produced no result set.
        Some(header) => match last {
            Err(e) => {
                let _ = header.send(Err(e));
            }
            Ok(end) => {
                if header.send(Ok(columns)).is_ok() {
                    let _ = rows.send(Ok(end)).await;
                }
            }
        },
        None => {
            let _ = rows.send(last).await;
        }
    }
}

fn build_proc<'a>(
    name: &str,
    params: Vec<(String, DataValue)>,
    legacy_datetime: bool,
) -> tds_driver::Command<'a> {
    let mut cmd = tds_driver::Command::new(name.to_string());
    for (pname, value) in params {
        match value {
            DataValue::DateTime(dt) if legacy_datetime => {
                cmd.bind_param(pname, LegacyDateTime(to_legacy_datetime(dt)));
            }
            value => cmd.bind_param(pname, value),
        }
    }
    cmd
}

async fn run_proc_query(
    driver: &mut Driver,
    name: &str,
    params: Vec<(String, DataValue)>,
    legacy_datetime: bool,
) -> Result<DataSet> {
    let mut stream = build_proc(name, params, legacy_datetime)
        .exec(driver)
        .await?;

    let mut dataset = DataSet::new();
    let mut current: Option<(DataTable, Arc<[Column]>)> = None;
    // As in `collect_query_stream`.
    let mut failure: Option<Error> = None;

    while let Some(item) = stream.next().await {
        match item {
            Ok(tds_driver::CommandItem::Metadata(meta)) => {
                if let Some((table, _)) = current.take() {
                    dataset.push(table);
                }
                let columns = columns_of(&meta);
                let tname = format!("table{}", meta.result_index());
                current = Some((DataTable::new(tname, Arc::clone(&columns)), columns));
            }
            Ok(tds_driver::CommandItem::Row(row)) if failure.is_none() => {
                let (table, columns) = current.get_or_insert_with(|| {
                    let columns: Arc<[Column]> = Arc::from(Vec::new());
                    (DataTable::new("table0", Arc::clone(&columns)), columns)
                });
                match row_of(row, columns) {
                    Ok(row) => table.push(row),
                    Err(e) => failure = Some(e),
                }
            }
            Ok(tds_driver::CommandItem::Row(_)) => {}
            Ok(_) => {}
            // The driver holds a server error back until it has read to the
            // end of the response, so nothing follows this.
            Err(e) => {
                failure.get_or_insert(e.into());
                break;
            }
        }
    }

    if let Some(e) = failure {
        return Err(e);
    }
    if let Some((table, _)) = current.take() {
        dataset.push(table);
    }
    Ok(dataset)
}

async fn run_proc_execute(
    driver: &mut Driver,
    name: &str,
    params: Vec<(String, DataValue)>,
    legacy_datetime: bool,
) -> Result<u64> {
    let mut stream = build_proc(name, params, legacy_datetime)
        .exec(driver)
        .await?;
    let mut total = 0u64;
    while let Some(item) = stream.next().await {
        if let tds_driver::CommandItem::RowsAffected(n) = item? {
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
///
/// # Cancelling
///
/// Stop waiting for a request -- drop its future, let a `timeout` or
/// `select!` give up on it, abort the task running it, drop a [`RowStream`]
/// early -- and it is cancelled on the server too, so it stops holding the
/// connection, the server's time and any locks it has taken. The connection
/// stays usable. A request still queued behind another never runs; one
/// already running is stopped where it is, even before it has sent anything
/// back, and the statements after it in the same batch don't run.
///
/// Commits and rollbacks always run, whatever became of their futures. And
/// cancelling stops a statement, not the transaction around it: work finished
/// before the cancel stays done, as it would if the statement had failed, so
/// put statements that must stand or fall together in a [`Transaction`]. The
/// exception is a session with `SET XACT_ABORT ON`, where the server treats a
/// cancel like any other statement failure and rolls the whole transaction
/// back; the `Transaction` then reports [`Error::TransactionAborted`].
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
    ///
    /// Gives up after the configuration's connect timeout, 15 seconds unless
    /// set. A named instance in a connection string without a port
    /// (`Server=host\INSTANCE`) is found through SQL Server Browser; the
    /// builder's [`host`](Config::host) is a plain host name. A redirect to
    /// another server, as Azure SQL's gateway sends, is followed.
    pub async fn connect(config: &Config) -> Result<Self> {
        let cfg = config.to_driver()?;
        let server = cfg.get_addr();
        let limit = cfg.connect_timeout();
        let open = open(cfg);
        let opened = match limit {
            Some(limit) => tokio::time::timeout(limit, open).await.unwrap_or_else(|_| {
                Err(tds_driver::error::Error::Io {
                    kind: std::io::ErrorKind::TimedOut,
                    message: format!("no connection within {limit:?}"),
                })
            }),
            None => open.await,
        };
        let driver = opened.map_err(|e| Error::Connect {
            server,
            source: Box::new(e),
        })?;

        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(connection_task(driver, rx, config.legacy_datetime()));

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

    /// Run a query and receive the rows of its first result set as the server
    /// sends them, rather than all at once after the last one arrives.
    ///
    /// Returns as soon as the server has described the columns, so a statement
    /// the server rejects fails here, just as it would from
    /// [`query`](Self::query). A failure partway through arrives as the
    /// stream's last item instead, after the rows that came before it.
    ///
    /// The stream borrows the client until it is dropped; see [`RowStream`] for
    /// what that holds up, and what happens if it is dropped early.
    ///
    /// ```no_run
    /// # use tdsql::Client;
    /// # async fn f(client: &mut Client) -> tdsql::Result<()> {
    /// let mut rows = client
    ///     .query_stream("SELECT id, name FROM users WHERE id > @P1", &[&10i32])
    ///     .await?;
    ///
    /// while let Some(row) = rows.try_next().await? {
    ///     let name: String = row.get("name");
    ///     println!("{name}");
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn query_stream(
        &mut self,
        sql: &str,
        params: &[&dyn ToSql],
    ) -> Result<RowStream<'_>> {
        let (sql, params) = (sql.to_string(), to_values(params));
        let (rows, receiver) = mpsc::channel(STREAM_BUFFER);
        let columns = self
            .call(|header| Request::QueryStream {
                sql,
                params,
                header,
                rows,
            })
            .await?;
        Ok(RowStream::new(columns, receiver))
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

    /// Run a query that returns one row, and read its first column.
    ///
    /// Use `Option<T>` as `T` to accept a `NULL`. Fails with
    /// [`Error::UnexpectedRowCount`] unless the query returns exactly one row,
    /// as [`query_one`](Self::query_one) does.
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
    /// If the batch holds several statements, the counts are summed. A
    /// statement run under `SET NOCOUNT ON`, as most stored procedures are,
    /// reports no count at all, so the total is `0` whatever it changed;
    /// rows a trigger changes are counted along with the statement's own.
    ///
    /// With no parameters the SQL is sent as a plain batch, so a `SET` option
    /// or `BEGIN TRAN` takes effect on the session, as it would from `batch`.
    /// With parameters it goes through `sp_executesql`, whose `SET` options
    /// last only for that call.
    pub async fn execute(&mut self, sql: &str, params: &[&dyn ToSql]) -> Result<u64> {
        let (sql, params) = (sql.to_string(), to_values(params));
        self.call(|reply| Request::Execute { sql, params, reply })
            .await
    }

    /// Run a SQL batch verbatim, with no parameters, and collect its rows.
    ///
    /// The same wire form as [`execute`](Self::execute) or [`query`](Self::query)
    /// with no parameters, which also go as a plain batch. This one makes the
    /// absence of parameters explicit, and returns every row whatever result
    /// set it came from, so it suits DDL and scripts: `CREATE PROCEDURE`,
    /// `CREATE VIEW` and `CREATE TRIGGER` must be the first statement of their
    /// batch, which a parameterised call, sent through `sp_executesql`, cannot
    /// give them.
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

    /// Begin a transaction at `READ COMMITTED`, SQL Server's default isolation
    /// level.
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
        // Not `Unspecified`, which would keep whatever level the connection
        // was left at.
        Transaction::begin(self, IsolationLevel::ReadCommitted).await
    }

    /// Begin a transaction at an explicit isolation level.
    ///
    /// Once it ends, the connection goes back to `READ COMMITTED`.
    pub async fn transaction_with_isolation(
        &mut self,
        level: IsolationLevel,
    ) -> Result<Transaction<'_>> {
        Transaction::begin(self, level).await
    }

    /// Close the connection.
    ///
    /// The same as dropping the client: the connection task ends and the
    /// socket is closed. Nothing more is sent to the server, which treats a
    /// closed socket as the end of the session.
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
