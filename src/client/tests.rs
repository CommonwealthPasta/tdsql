use super::*;
use tiberius::ColumnData as C;

#[test]
fn maps_primitive_columns() {
    assert_eq!(
        map_column_data(C::I32(Some(5)), "c").unwrap(),
        DataValue::Int(5)
    );
    assert_eq!(
        map_column_data(C::I64(Some(5)), "c").unwrap(),
        DataValue::BigInt(5)
    );
    assert_eq!(
        map_column_data(C::U8(Some(5)), "c").unwrap(),
        DataValue::TinyInt(5)
    );
    assert_eq!(
        map_column_data(C::I16(Some(5)), "c").unwrap(),
        DataValue::SmallInt(5)
    );
    assert_eq!(
        map_column_data(C::F64(Some(2.5)), "c").unwrap(),
        DataValue::Float(2.5)
    );
    assert_eq!(
        map_column_data(C::Bit(Some(true)), "c").unwrap(),
        DataValue::Bool(true)
    );
    assert_eq!(
        map_column_data(C::String(Some("hi".into())), "c").unwrap(),
        DataValue::Text("hi".into())
    );
}

#[test]
fn widens_f32_to_float() {
    assert_eq!(
        map_column_data(C::F32(Some(2.5)), "c").unwrap(),
        DataValue::Float(2.5)
    );
}

#[test]
fn maps_every_null_to_null() {
    assert!(map_column_data(C::I32(None), "c").unwrap().is_null());
    assert!(map_column_data(C::String(None), "c").unwrap().is_null());
    assert!(map_column_data(C::Bit(None), "c").unwrap().is_null());
    assert!(map_column_data(C::Numeric(None), "c").unwrap().is_null());
    assert!(map_column_data(C::DateTime(None), "c").unwrap().is_null());
    assert!(map_column_data(C::Date(None), "c").unwrap().is_null());
}

#[test]
fn round_trips_a_value_through_binding() {
    // Binding and reading back must agree, or parameters silently change type.
    for value in [
        DataValue::Int(7),
        DataValue::BigInt(8),
        DataValue::Bool(true),
        DataValue::Float(1.5),
        DataValue::Text("hi".into()),
        DataValue::Binary(vec![1, 2, 3]),
    ] {
        let bound = tiberius::ToSql::to_sql(&value);
        let read = map_column_data(bound, "c").unwrap();
        assert_eq!(read, value, "round-trip changed {value:?}");
    }
}

#[test]
fn owned_binding_matches_borrowed_binding() {
    for value in [
        DataValue::Int(7),
        DataValue::Text("hi".into()),
        DataValue::Binary(vec![1, 2]),
        DataValue::Null,
    ] {
        let borrowed = map_column_data(tiberius::ToSql::to_sql(&value), "c").unwrap();
        let owned = map_column_data(tiberius::IntoSql::into_sql(value.clone()), "c").unwrap();
        assert_eq!(borrowed, owned, "binding paths disagree for {value:?}");
    }
}

#[test]
fn converts_positional_params_to_values() {
    let params: &[&dyn ToSql] = &[&7i32, &"hi", &true];
    assert_eq!(
        to_values(params),
        vec![
            DataValue::Int(7),
            DataValue::Text("hi".into()),
            DataValue::Bool(true)
        ]
    );
}

#[test]
fn null_binds_as_an_untyped_int() {
    // Documented limitation: an untyped NULL goes out as a NULL int.
    assert!(matches!(
        tiberius::ToSql::to_sql(&DataValue::Null),
        C::I32(None)
    ));
}
