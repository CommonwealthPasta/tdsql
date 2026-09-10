use super::*;

fn get<T: FromSql>(v: DataValue) -> Result<T> {
    T::from_sql(&v, "col")
}

#[test]
fn reads_each_primitive() {
    assert_eq!(get::<u8>(DataValue::TinyInt(3)).unwrap(), 3u8);
    assert_eq!(get::<i16>(DataValue::SmallInt(-4)).unwrap(), -4i16);
    assert_eq!(get::<i32>(DataValue::Int(5)).unwrap(), 5i32);
    assert_eq!(get::<i64>(DataValue::BigInt(6)).unwrap(), 6i64);
    assert_eq!(get::<f64>(DataValue::Float(2.5)).unwrap(), 2.5);
    assert!(get::<bool>(DataValue::Bool(true)).unwrap());
    assert_eq!(get::<String>(DataValue::Text("hi".into())).unwrap(), "hi");
    assert_eq!(
        get::<Vec<u8>>(DataValue::Binary(vec![1, 2])).unwrap(),
        vec![1, 2]
    );
}

#[test]
fn integers_widen_but_never_narrow() {
    // A tinyint column reads fine as any wider integer.
    assert_eq!(get::<i32>(DataValue::TinyInt(3)).unwrap(), 3);
    assert_eq!(get::<i64>(DataValue::SmallInt(3)).unwrap(), 3);
    assert_eq!(get::<i64>(DataValue::Int(3)).unwrap(), 3);

    // But a bigint must not silently truncate into an i32.
    assert!(get::<i32>(DataValue::BigInt(3)).is_err());
    assert!(get::<u8>(DataValue::Int(3)).is_err());
}

#[test]
fn null_reads_as_none() {
    assert_eq!(get::<Option<i32>>(DataValue::Null).unwrap(), None);
    assert_eq!(get::<Option<i32>>(DataValue::Int(1)).unwrap(), Some(1));
    assert_eq!(get::<Option<String>>(DataValue::Null).unwrap(), None);
}

#[test]
fn null_into_non_optional_is_an_error() {
    let err = get::<i32>(DataValue::Null).unwrap_err();
    assert!(matches!(err, Error::Conversion { .. }));
    assert_eq!(
        err.to_string(),
        "cannot convert column 'col' from Null to i32"
    );
}

#[test]
fn mismatched_type_reports_both_sides() {
    let err = get::<i32>(DataValue::Text("x".into())).unwrap_err();
    assert_eq!(
        err.to_string(),
        "cannot convert column 'col' from Text to i32"
    );
}

#[test]
fn binds_native_types() {
    assert_eq!(7i32.to_value(), DataValue::Int(7));
    assert_eq!("hi".to_value(), DataValue::Text("hi".into()));
    assert_eq!(true.to_value(), DataValue::Bool(true));
    assert_eq!(2.5f32.to_value(), DataValue::Float(2.5));
}

#[test]
fn binds_option_as_null() {
    assert_eq!(None::<i32>.to_value(), DataValue::Null);
    assert_eq!(Some(3i32).to_value(), DataValue::Int(3));
}

#[test]
fn from_option_yields_null() {
    assert_eq!(DataValue::from(None::<i32>), DataValue::Null);
    assert_eq!(DataValue::from(Some(3i32)), DataValue::Int(3));
}

#[test]
fn comparisons_widen_across_integer_widths() {
    assert_eq!(DataValue::TinyInt(1), 1i32);
    assert_eq!(DataValue::SmallInt(1), 1i32);
    assert_eq!(DataValue::Int(1), 1i32);
    assert_eq!(DataValue::BigInt(1), 1i32);
    // These did not compile before: there was no PartialEq for i64, i16 or u8.
    assert_eq!(DataValue::BigInt(4), 4i64);
    assert_eq!(DataValue::SmallInt(4), 4i16);
    assert_eq!(DataValue::TinyInt(4), 4u8);
}

#[test]
fn comparisons_reject_other_variants() {
    assert_ne!(DataValue::Text("1".into()), 1i32);
    assert_ne!(DataValue::Null, 1i32);
    assert_ne!(DataValue::Bool(true), 1i32);
}

#[test]
fn null_is_the_default() {
    assert_eq!(DataValue::default(), DataValue::Null);
    assert!(DataValue::Null.is_null());
    assert!(!DataValue::Int(0).is_null());
}

#[test]
fn type_names_are_stable() {
    assert_eq!(DataValue::Int(1).type_name(), "Int");
    assert_eq!(DataValue::Null.type_name(), "Null");
    assert_eq!(DataValue::Text(String::new()).type_name(), "Text");
}

#[test]
fn maps_driver_column_types() {
    assert_eq!(SqlType::from(tiberius::ColumnType::Int4), SqlType::Int);
    assert_eq!(
        SqlType::from(tiberius::ColumnType::Numericn),
        SqlType::Numeric
    );
    assert_eq!(
        SqlType::from(tiberius::ColumnType::BigVarBin),
        SqlType::VarBinary
    );
    assert_eq!(
        SqlType::from(tiberius::ColumnType::NVarchar),
        SqlType::NVarChar
    );
}

#[test]
fn binds_any_timezone_as_datetimeoffset() {
    // The same instant in three zones must bind to the same offset value.
    let utc = Utc.with_ymd_and_hms(2024, 3, 1, 12, 0, 0).unwrap();
    let plus_two = FixedOffset::east_opt(2 * 3600).unwrap();
    let aware = utc.with_timezone(&plus_two);

    let expected = DataValue::DateTimeOffset(utc.fixed_offset());
    assert_eq!(utc.to_value(), expected);
    assert_eq!(aware.to_value(), expected);
    assert_eq!(utc.with_timezone(&Local).to_value(), expected);

    // ...and `From` agrees with `ToSql`.
    assert_eq!(DataValue::from(utc), expected);
    assert_eq!(DataValue::from(aware), expected);
}

#[test]
fn binding_a_zoned_timestamp_keeps_the_wall_clock_of_its_zone() {
    // 14:00 at +02:00 is the same instant as 12:00Z, and that is what goes out.
    let plus_two = FixedOffset::east_opt(2 * 3600).unwrap();
    let aware = plus_two.with_ymd_and_hms(2024, 3, 1, 14, 0, 0).unwrap();

    let DataValue::DateTimeOffset(sent) = aware.to_value() else {
        panic!("expected a datetimeoffset");
    };
    assert_eq!(sent.naive_local().to_string(), "2024-03-01 14:00:00");
    assert_eq!(sent.offset().local_minus_utc(), 2 * 3600);
    assert_eq!(sent.to_utc().to_string(), "2024-03-01 12:00:00 UTC");
}

#[test]
fn reads_datetimeoffset_into_any_zone() {
    let plus_two = FixedOffset::east_opt(2 * 3600).unwrap();
    let aware = plus_two.with_ymd_and_hms(2024, 3, 1, 14, 0, 0).unwrap();
    let stored = DataValue::DateTimeOffset(aware);

    // Reading re-projects the zone but names the same instant.
    assert_eq!(
        get::<DateTime<Utc>>(stored.clone()).unwrap(),
        Utc.with_ymd_and_hms(2024, 3, 1, 12, 0, 0).unwrap()
    );
    assert_eq!(get::<DateTime<Local>>(stored.clone()).unwrap(), aware);
    assert_eq!(get::<DateTime<FixedOffset>>(stored.clone()).unwrap(), aware);
    assert_eq!(get::<Option<DateTime<Utc>>>(DataValue::Null).unwrap(), None);
}

#[test]
fn a_naive_datetime_is_not_silently_called_utc() {
    // `datetime2` carries no offset, so reading one as an aware type would be a
    // guess. It stays an error; the caller picks the zone explicitly.
    let naive = NaiveDate::from_ymd_opt(2024, 3, 1)
        .unwrap()
        .and_hms_opt(12, 0, 0)
        .unwrap();
    let err = get::<DateTime<Utc>>(DataValue::DateTime(naive)).unwrap_err();
    assert_eq!(
        err.to_string(),
        "cannot convert column 'col' from DateTime to DateTime<Utc>"
    );

    // The explicit route works, and is one call.
    assert_eq!(
        naive.and_utc().to_value(),
        DataValue::DateTimeOffset(naive.and_utc().fixed_offset())
    );
}

#[test]
fn compares_equal_across_zones() {
    let utc = Utc.with_ymd_and_hms(2024, 3, 1, 12, 0, 0).unwrap();
    let plus_two = FixedOffset::east_opt(2 * 3600).unwrap();
    let value = DataValue::DateTimeOffset(utc.fixed_offset());

    assert_eq!(value, utc);
    assert_eq!(value, utc.with_timezone(&plus_two));
    assert_ne!(value, utc + chrono::Duration::hours(1));
    assert_ne!(DataValue::Null, utc);
}
