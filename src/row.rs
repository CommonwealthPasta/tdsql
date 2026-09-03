//! Result rows and their column metadata.

use std::ops::Index;
use std::sync::Arc;

use crate::error::{Error, Result};
use crate::value::{DataValue, FromSql, SqlType};

/// Metadata for one column of a result set.
///
/// TDS reports the column name and type on the result-set metadata token, but
/// not nullability or declared size, so those are deliberately absent rather
/// than guessed at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    name: String,
    sql_type: SqlType,
}

impl Column {
    /// Construct a column descriptor.
    pub fn new(name: impl Into<String>, sql_type: SqlType) -> Self {
        Self {
            name: name.into(),
            sql_type,
        }
    }

    /// The column name as reported by the server.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The SQL Server type of the column.
    pub fn sql_type(&self) -> SqlType {
        self.sql_type
    }
}

/// A value that can address a column: either a positional `usize` or a `&str`
/// name.
///
/// This is what lets [`Row::get`] serve both `row.get("id")` and `row.get(0)`.
pub trait RowIndex {
    /// Resolve to a column position within `columns`, or fail.
    fn index_of(&self, columns: &[Column]) -> Result<usize>;
    /// A label for this index, used in error messages.
    fn label(&self) -> String;
}

impl RowIndex for usize {
    fn index_of(&self, columns: &[Column]) -> Result<usize> {
        if *self < columns.len() {
            Ok(*self)
        } else {
            Err(Error::ColumnIndexOutOfRange {
                index: *self,
                len: columns.len(),
            })
        }
    }

    fn label(&self) -> String {
        self.to_string()
    }
}

impl RowIndex for str {
    fn index_of(&self, columns: &[Column]) -> Result<usize> {
        columns
            .iter()
            .position(|c| c.name == self)
            .ok_or_else(|| Error::ColumnNotFound(self.to_string()))
    }

    fn label(&self) -> String {
        self.to_string()
    }
}

impl<T: RowIndex + ?Sized> RowIndex for &T {
    fn index_of(&self, columns: &[Column]) -> Result<usize> {
        (**self).index_of(columns)
    }

    fn label(&self) -> String {
        (**self).label()
    }
}

/// One row of a result set.
///
/// Values are stored positionally, so column order is preserved and duplicate
/// column names (`SELECT a, a`) stay addressable by index. Column metadata is
/// shared across every row of a result set rather than cloned per row.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    columns: Arc<[Column]>,
    values: Vec<DataValue>,
}

impl Row {
    /// Construct a row from shared column metadata and its values.
    pub fn new(columns: Arc<[Column]>, values: Vec<DataValue>) -> Self {
        Self { columns, values }
    }

    /// The columns of this row's result set.
    pub fn columns(&self) -> &[Column] {
        &self.columns
    }

    /// The number of columns.
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Whether the row has no columns.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// The raw values, in column order.
    pub fn values(&self) -> &[DataValue] {
        &self.values
    }

    /// Get a typed value by column name or position.
    ///
    /// # Panics
    ///
    /// Panics if the column does not exist or the value is not convertible to
    /// `T`. Use [`try_get`](Self::try_get) to handle those cases.
    ///
    /// ```no_run
    /// # fn f(row: tdsql::Row) {
    /// let id: i32 = row.get("id");
    /// let name: String = row.get(1);
    /// # }
    /// ```
    pub fn get<T: FromSql, I: RowIndex>(&self, idx: I) -> T {
        match self.try_get(&idx) {
            Ok(v) => v,
            Err(e) => panic!("failed to get column {}: {e}", idx.label()),
        }
    }

    /// Get a typed value by column name or position, returning an error rather
    /// than panicking.
    pub fn try_get<T: FromSql, I: RowIndex>(&self, idx: I) -> Result<T> {
        let pos = idx.index_of(&self.columns)?;
        T::from_sql(&self.values[pos], self.columns[pos].name())
    }

    /// Borrow the raw value at a column, if that column exists.
    pub fn value<I: RowIndex>(&self, idx: I) -> Option<&DataValue> {
        idx.index_of(&self.columns).ok().map(|i| &self.values[i])
    }
}

impl Index<&str> for Row {
    type Output = DataValue;

    fn index(&self, column: &str) -> &Self::Output {
        match column.index_of(&self.columns) {
            Ok(i) => &self.values[i],
            Err(e) => panic!("{e}"),
        }
    }
}

impl Index<usize> for Row {
    type Output = DataValue;

    fn index(&self, column: usize) -> &Self::Output {
        match column.index_of(&self.columns) {
            Ok(i) => &self.values[i],
            Err(e) => panic!("{e}"),
        }
    }
}

#[cfg(test)]
mod tests;
