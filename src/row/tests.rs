use super::*;
use crate::value::DataValue;

fn sample() -> Row {
    let columns: Arc<[Column]> = Arc::from(vec![
        Column::new("id", SqlType::Int),
        Column::new("name", SqlType::NVarChar),
        Column::new("note", SqlType::NVarChar),
    ]);
    Row::new(
        columns,
        vec![
            DataValue::Int(7),
            DataValue::Text("Ada".into()),
            DataValue::Null,
        ],
    )
}

#[test]
fn gets_by_name_and_by_index() {
    let row = sample();
    assert_eq!(row.get::<i32, _>("id"), 7);
    assert_eq!(row.get::<String, _>("name"), "Ada");
    assert_eq!(row.get::<i32, _>(0), 7);
    assert_eq!(row.get::<String, _>(1), "Ada");
}

#[test]
fn try_get_reports_a_missing_column() {
    let row = sample();
    let err = row.try_get::<i32, _>("nope").unwrap_err();
    assert!(matches!(err, Error::ColumnNotFound(ref c) if c == "nope"));
    assert_eq!(err.to_string(), "column 'nope' not found");
}

#[test]
fn try_get_reports_an_out_of_range_index() {
    let row = sample();
    let err = row.try_get::<i32, _>(9usize).unwrap_err();
    assert_eq!(err.to_string(), "column index 9 out of range (3 columns)");
}

#[test]
fn try_get_reports_a_bad_conversion() {
    let row = sample();
    let err = row.try_get::<i32, _>("name").unwrap_err();
    assert_eq!(
        err.to_string(),
        "cannot convert column 'name' from Text to i32"
    );
}

#[test]
fn null_reads_as_none() {
    let row = sample();
    assert_eq!(row.try_get::<Option<String>, _>("note").unwrap(), None);
    assert!(row.try_get::<String, _>("note").is_err());
}

#[test]
fn preserves_column_order() {
    let row = sample();
    let names: Vec<_> = row.columns().iter().map(Column::name).collect();
    assert_eq!(names, ["id", "name", "note"]);
    assert_eq!(row.len(), 3);
    assert!(!row.is_empty());
}

#[test]
fn duplicate_column_names_stay_addressable_by_index() {
    // SELECT a, a used to collapse into a single HashMap entry.
    let columns: Arc<[Column]> = Arc::from(vec![
        Column::new("a", SqlType::Int),
        Column::new("a", SqlType::Int),
    ]);
    let row = Row::new(columns, vec![DataValue::Int(1), DataValue::Int(2)]);

    assert_eq!(row.len(), 2);
    assert_eq!(row.get::<i32, _>(0), 1);
    assert_eq!(row.get::<i32, _>(1), 2);
    // By name, the first match wins.
    assert_eq!(row.get::<i32, _>("a"), 1);
}

#[test]
fn indexing_yields_raw_values() {
    let row = sample();
    assert_eq!(row["id"], 7i32);
    assert_eq!(row[1], DataValue::Text("Ada".into()));
    assert!(row["note"].is_null());
}

#[test]
fn value_returns_none_for_missing_columns() {
    let row = sample();
    assert!(row.value("nope").is_none());
    assert!(row.value(99usize).is_none());
    assert_eq!(row.value("id"), Some(&DataValue::Int(7)));
}

#[test]
#[should_panic(expected = "column 'nope' not found")]
fn get_panics_on_a_missing_column() {
    sample().get::<i32, _>("nope");
}

#[test]
fn column_exposes_its_type() {
    let c = Column::new("id", SqlType::Int);
    assert_eq!(c.name(), "id");
    assert_eq!(c.sql_type(), SqlType::Int);
}
