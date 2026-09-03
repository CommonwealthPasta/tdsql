//! The connection: connect once, run many statements.

use std::borrow::Cow;
use std::sync::Arc;

use futures::StreamExt;
use tokio::net::TcpStream;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

use crate::command::{Command, Prepared};
use crate::config::Config;
use crate::dataset::{DataSet, DataTable};
use crate::error::{Error, Result};
use crate::row::{Column, Row};
use crate::value::{DataValue, FromSql, SqlType, ToSql};

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
            // Every other variant is `Copy`-ish and borrows nothing.
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
/// This is the crate's single mapping path: both the row-returning and the
/// scalar-returning queries go through it, so the two can never drift.
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

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// A connection to SQL Server.
///
/// Connect once and reuse it; each statement is sent over the same connection.
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
    inner: tiberius::Client<Compat<TcpStream>>,
}

impl Client {
    /// Connect using this configuration.
    pub async fn connect(config: &Config) -> Result<Self> {
        let cfg = config.to_tiberius()?;
        let addr = cfg.get_addr();
        let tcp = TcpStream::connect(&addr).await?;
        tcp.set_nodelay(true)?;
        let inner = tiberius::Client::connect(cfg, tcp.compat_write())
            .await
            .map_err(|e| Error::Connect {
                server: addr,
                source: Box::new(e),
            })?;
        Ok(Self { inner })
    }

    /// Connect using an ADO.NET-style connection string.
    pub async fn connect_str(connection_string: &str) -> Result<Self> {
        Self::connect(&Config::from_ado_string(connection_string)).await
    }

    /// Run a query and collect every row of the first result set.
    ///
    /// Parameters are positional: `@P1` is `params[0]`.
    pub async fn query(&mut self, sql: &str, params: &[&dyn ToSql]) -> Result<Vec<Row>> {
        let ds = self.query_values(sql, &to_values(params)).await?;
        Ok(ds.into_rows())
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
        let values = to_values(params);
        let refs = as_tiberius_refs(&values);
        Ok(self.inner.execute(sql, &refs).await?.total())
    }

    /// Run a [`Command`] and collect every result set it produces.
    ///
    /// This is the path for stored procedures and for batches that return more
    /// than one result set.
    pub async fn query_dataset(&mut self, command: &Command) -> Result<DataSet> {
        match command.prepare() {
            Prepared::Text { sql, params } => self.query_values(&sql, &params).await,
            Prepared::Proc { name, params } => self.proc_dataset(&name, params).await,
        }
    }

    /// Run a [`Command`] for its row count rather than its rows.
    pub async fn execute_command(&mut self, command: &Command) -> Result<u64> {
        match command.prepare() {
            Prepared::Text { sql, params } => {
                let refs = as_tiberius_refs(&params);
                Ok(self.inner.execute(&sql, &refs).await?.total())
            }
            Prepared::Proc { name, params } => {
                let mut cmd = tiberius::Command::new(name);
                for (pname, value) in params {
                    cmd.bind_param(pname, value);
                }
                let mut stream = cmd.exec(&mut self.inner).await?;
                let mut total = 0u64;
                while let Some(item) = stream.next().await {
                    if let tiberius::CommandItem::RowsAffected(n) = item? {
                        total += n;
                    }
                }
                Ok(total)
            }
        }
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
        let stream = self.inner.simple_query(sql).await?;
        Ok(collect_query_stream(stream).await?.into_rows())
    }

    /// Run a SQL batch verbatim and collect every result set it produces.
    pub async fn batch_dataset(&mut self, sql: &str) -> Result<DataSet> {
        let stream = self.inner.simple_query(sql).await?;
        collect_query_stream(stream).await
    }

    /// Drive a text query to a full `DataSet`.
    async fn query_values(&mut self, sql: &str, params: &[DataValue]) -> Result<DataSet> {
        let refs = as_tiberius_refs(params);
        let stream = self.inner.query(sql, &refs).await?;
        collect_query_stream(stream).await
    }

    /// Drive a stored-procedure RPC to a full `DataSet`.
    async fn proc_dataset(
        &mut self,
        name: &str,
        params: Vec<(String, DataValue)>,
    ) -> Result<DataSet> {
        let mut cmd = tiberius::Command::new(name.to_string());
        for (pname, value) in params {
            cmd.bind_param(pname, value);
        }
        let mut stream = cmd.exec(&mut self.inner).await?;

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

    /// Close the connection, sending a graceful logout.
    pub async fn close(self) -> Result<()> {
        self.inner.close().await?;
        Ok(())
    }
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

fn to_values(params: &[&dyn ToSql]) -> Vec<DataValue> {
    params.iter().map(|p| p.to_value()).collect()
}

fn as_tiberius_refs(values: &[DataValue]) -> Vec<&dyn tiberius::ToSql> {
    values.iter().map(|v| v as &dyn tiberius::ToSql).collect()
}

#[cfg(test)]
mod tests;
