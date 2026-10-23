use super::*;
use tds_driver::ColumnData as C;

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
        let bound = tds_driver::ToSql::to_sql(&value);
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
        let borrowed = map_column_data(tds_driver::ToSql::to_sql(&value), "c").unwrap();
        let owned = map_column_data(tds_driver::IntoSql::into_sql(value.clone()), "c").unwrap();
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
fn a_typed_null_binds_as_a_null_of_its_type() {
    for (ty, expected) in [
        (SqlType::Bit, C::Bit(None)),
        (SqlType::BitN, C::Bit(None)),
        (SqlType::TinyInt, C::U8(None)),
        (SqlType::SmallInt, C::I16(None)),
        (SqlType::Int, C::I32(None)),
        (SqlType::BigInt, C::I64(None)),
        (SqlType::IntN, C::I64(None)),
        (SqlType::Real, C::F32(None)),
        (SqlType::Float, C::F64(None)),
        (SqlType::FloatN, C::F64(None)),
        (SqlType::Decimal, C::Numeric(None)),
        (SqlType::Numeric, C::Numeric(None)),
        (SqlType::Money, C::Numeric(None)),
        (SqlType::SmallMoney, C::Numeric(None)),
        (SqlType::Guid, C::Guid(None)),
        (SqlType::Date, C::Date(None)),
        (SqlType::Time, C::Time(None)),
        (SqlType::DateTime, C::DateTime(None)),
        (SqlType::DateTimeN, C::DateTime(None)),
        (SqlType::SmallDateTime, C::SmallDateTime(None)),
        (SqlType::DateTime2, C::DateTime2(None)),
        (SqlType::DateTimeOffset, C::DateTimeOffset(None)),
        (SqlType::Binary, C::Binary(None)),
        (SqlType::VarBinary, C::Binary(None)),
        (SqlType::Image, C::Binary(None)),
        (SqlType::Xml, C::Xml(None)),
        (SqlType::Char, C::String(None)),
        (SqlType::VarChar, C::String(None)),
        (SqlType::NChar, C::String(None)),
        (SqlType::NVarChar, C::String(None)),
        (SqlType::Text, C::String(None)),
        (SqlType::NText, C::String(None)),
    ] {
        let value = DataValue::TypedNull(ty);
        assert_eq!(tds_driver::ToSql::to_sql(&value), expected, "{ty:?}");
        assert_eq!(tds_driver::IntoSql::into_sql(value), expected, "{ty:?}");
    }
}

#[test]
fn a_typed_null_with_no_parameter_type_binds_untyped() {
    let untyped = tds_driver::ToSql::to_sql(&DataValue::Null);
    for ty in [SqlType::Null, SqlType::Udt, SqlType::Variant] {
        assert_eq!(
            tds_driver::ToSql::to_sql(&DataValue::TypedNull(ty)),
            untyped,
            "{ty:?}"
        );
    }
}

#[test]
fn null_binds_as_an_untyped_int() {
    // Documented limitation: an untyped NULL goes out as a NULL int.
    assert!(matches!(
        tds_driver::ToSql::to_sql(&DataValue::Null),
        C::I32(None)
    ));
}
