use super::*;
use crate::value::{DataValue, SqlType};

fn table(name: &str, values: &[i32]) -> DataTable {
    let columns: Arc<[Column]> = Arc::from(vec![Column::new("v", SqlType::Int)]);
    let mut t = DataTable::new(name, Arc::clone(&columns));
    for v in values {
        t.push(Row::new(Arc::clone(&columns), vec![DataValue::Int(*v)]));
    }
    t
}

fn sample() -> DataSet {
    let mut ds = DataSet::new();
    ds.push(table("table0", &[1, 2]));
    ds.push(table("table1", &[3]));
    ds
}

#[test]
fn result_sets_keep_server_order() {
    let ds = sample();
    assert_eq!(ds.len(), 2);
    assert_eq!(ds.tables()[0].name(), "table0");
    assert_eq!(ds.tables()[1].name(), "table1");
}

#[test]
fn indexes_positionally() {
    // This is the shape that replaces the old ds.tables["table0"][0].
    let ds = sample();
    assert_eq!(ds[0][0]["v"], 1i32);
    assert_eq!(ds[0][1]["v"], 2i32);
    assert_eq!(ds[1][0]["v"], 3i32);
}

#[test]
fn looks_up_by_position_and_name() {
    let ds = sample();
    assert_eq!(ds.table(1).unwrap().name(), "table1");
    assert!(ds.table(9).is_none());
    assert_eq!(ds.table_named("table1").unwrap().len(), 1);
    assert!(ds.table_named("nope").is_none());
}

#[test]
fn into_rows_takes_the_first_result_set() {
    let rows = sample().into_rows();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["v"], 1i32);
}

#[test]
fn into_rows_on_an_empty_set_is_empty() {
    assert!(DataSet::new().into_rows().is_empty());
}

#[test]
fn empty_set_reports_empty() {
    let ds = DataSet::new();
    assert!(ds.is_empty());
    assert_eq!(ds.len(), 0);
    assert!(ds.table(0).is_none());
}

#[test]
fn table_exposes_columns_and_rows() {
    let t = table("t", &[1, 2, 3]);
    assert_eq!(t.len(), 3);
    assert!(!t.is_empty());
    assert_eq!(t.columns().len(), 1);
    assert_eq!(t.rows().len(), 3);
    assert_eq!(t.into_rows().len(), 3);
}
