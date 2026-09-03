//! Multiple result sets, in the order the server returned them.

use std::ops::Index;
use std::sync::Arc;

use crate::row::{Column, Row};

/// One result set: its columns and its rows.
#[derive(Debug, Clone, PartialEq)]
pub struct DataTable {
    name: String,
    columns: Arc<[Column]>,
    rows: Vec<Row>,
}

impl DataTable {
    /// Construct a table from its name and shared column metadata.
    pub fn new(name: impl Into<String>, columns: Arc<[Column]>) -> Self {
        Self {
            name: name.into(),
            columns,
            rows: Vec::new(),
        }
    }

    /// The name of this result set (`table0`, `table1`, ... by position).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The columns of this result set.
    pub fn columns(&self) -> &[Column] {
        &self.columns
    }

    /// The rows, in the order the server returned them.
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// The number of rows.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether this result set has no rows.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Take ownership of the rows.
    pub fn into_rows(self) -> Vec<Row> {
        self.rows
    }

    pub(crate) fn push(&mut self, row: Row) {
        self.rows.push(row);
    }
}

impl Index<usize> for DataTable {
    type Output = Row;

    fn index(&self, index: usize) -> &Self::Output {
        &self.rows[index]
    }
}

/// Every result set produced by one command, in order.
///
/// Most queries return a single result set, in which case
/// [`Client::query`](crate::Client::query) hands back `Vec<Row>` directly and
/// you never need this type. Reach for `DataSet` when a batch or stored
/// procedure genuinely produces more than one.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DataSet {
    tables: Vec<DataTable>,
}

impl DataSet {
    /// An empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// The result sets, in server order.
    pub fn tables(&self) -> &[DataTable] {
        &self.tables
    }

    /// The result set at `index`, if present.
    pub fn table(&self, index: usize) -> Option<&DataTable> {
        self.tables.get(index)
    }

    /// The result set with this name, if present.
    pub fn table_named(&self, name: &str) -> Option<&DataTable> {
        self.tables.iter().find(|t| t.name() == name)
    }

    /// The number of result sets.
    pub fn len(&self) -> usize {
        self.tables.len()
    }

    /// Whether there are no result sets.
    pub fn is_empty(&self) -> bool {
        self.tables.is_empty()
    }

    /// Take the rows of the first result set, discarding any others.
    pub fn into_rows(self) -> Vec<Row> {
        self.tables
            .into_iter()
            .next()
            .map(DataTable::into_rows)
            .unwrap_or_default()
    }

    /// Take ownership of every result set.
    pub fn into_tables(self) -> Vec<DataTable> {
        self.tables
    }

    pub(crate) fn push(&mut self, table: DataTable) {
        self.tables.push(table);
    }
}

impl Index<usize> for DataSet {
    type Output = DataTable;

    fn index(&self, index: usize) -> &Self::Output {
        &self.tables[index]
    }
}

#[cfg(test)]
mod tests;
