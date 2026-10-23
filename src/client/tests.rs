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
fn a_wide_numeric_reads_once_its_trailing_zeros_go() {
    use rust_decimal::Decimal;
    use tds_driver::numeric::Numeric;

    // 0.5 in a decimal(38, 30): scale 30 is past Decimal's 28.
    let half = Numeric::new_with_scale(5 * 10i128.pow(29), 30);
    assert_eq!(
        map_column_data(C::Numeric(Some(half)), "c").unwrap(),
        DataValue::Decimal(Decimal::new(5, 1))
    );
    // 0 in a decimal(38, 38).
    let zero = Numeric::new_with_scale(0, 38);
    assert_eq!(
        map_column_data(C::Numeric(Some(zero)), "c").unwrap(),
        DataValue::Decimal(Decimal::ZERO)
    );
    // 1e11 in a decimal(38, 18): 29 digits, one too many until a zero goes.
    let big = Numeric::new_with_scale(10i128.pow(29), 18);
    assert_eq!(
        map_column_data(C::Numeric(Some(big)), "c").unwrap(),
        DataValue::Decimal(Decimal::new(100_000_000_000, 0))
    );
    // 10^30 at scale 0 has no zeros to spare at scale 0, so it still fails.
    let err = map_column_data(
        C::Numeric(Some(Numeric::new_with_scale(10i128.pow(30), 0))),
        "c",
    )
    .unwrap_err();
    assert!(matches!(err, Error::Decimal { .. }), "{err}");
    // 1.000...001 at scale 30 cannot drop its last digit, so it fails too.
    let err = map_column_data(
        C::Numeric(Some(Numeric::new_with_scale(10i128.pow(30) + 1, 30))),
        "c",
    )
    .unwrap_err();
    assert!(matches!(err, Error::Decimal { .. }), "{err}");
}

#[test]
fn an_untyped_null_binds_as_nvarchar() {
    assert_eq!(tds_driver::ToSql::to_sql(&DataValue::Null), C::String(None));
}

#[test]
fn legacy_datetime_rounds_to_the_nearest_tick() {
    use chrono::NaiveDate;

    let at = |ms: u32| {
        NaiveDate::from_ymd_opt(2024, 5, 1)
            .unwrap()
            .and_hms_milli_opt(10, 0, 0, ms)
            .unwrap()
    };
    let ticks = |dt| to_legacy_datetime(dt).seconds_fragments();
    // 10:00:00 is 36,000 s, 300 ticks each.
    assert_eq!(ticks(at(0)), 10_800_000);
    // .003 is tick 1, as the server stores it (.0033333); truncation would
    // give tick 0 and a value that matches nothing.
    assert_eq!(ticks(at(3)), 10_800_001);
    assert_eq!(ticks(at(7)), 10_800_002);
    assert_eq!(ticks(at(10)), 10_800_003);
    assert_eq!(ticks(at(997)), 10_800_299);
    assert_eq!(ticks(at(123)), 10_800_037);

    // Days count from 1900-01-01, either way.
    let base = NaiveDate::from_ymd_opt(1900, 1, 1).unwrap();
    assert_eq!(
        to_legacy_datetime(base.and_hms_opt(0, 0, 0).unwrap()).days(),
        0
    );
    let before = NaiveDate::from_ymd_opt(1899, 12, 31).unwrap();
    assert_eq!(
        to_legacy_datetime(before.and_hms_opt(12, 0, 0).unwrap()).days(),
        -1
    );

    // The last 100 ns of a day round up into the next one.
    let end = NaiveDate::from_ymd_opt(2024, 5, 1)
        .unwrap()
        .and_hms_nano_opt(23, 59, 59, 999_999_900)
        .unwrap();
    let rolled = to_legacy_datetime(end);
    assert_eq!(
        rolled.days(),
        (NaiveDate::from_ymd_opt(2024, 5, 2).unwrap() - base).num_days() as i32
    );
    assert_eq!(rolled.seconds_fragments(), 0);
}
