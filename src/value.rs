//! The dynamic value type, the SQL type tags, and the conversion traits.

use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime};
use rust_decimal::Decimal;
use uuid::Uuid;

use crate::error::{Error, Result};

/// A value read from, or bound to, SQL Server.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum DataValue {
    /// `tinyint`
    TinyInt(u8),
    /// `smallint`
    SmallInt(i16),
    /// `int`
    Int(i32),
    /// `bigint`
    BigInt(i64),
    /// `float` / `real`
    Float(f64),
    /// `decimal` / `numeric` / `money`
    Decimal(Decimal),
    /// `bit`
    Bool(bool),
    /// Any character or XML type.
    Text(String),
    /// Any binary type.
    Binary(Vec<u8>),
    /// `uniqueidentifier`
    Guid(Uuid),
    /// `date`
    Date(NaiveDate),
    /// `time`
    Time(NaiveTime),
    /// `datetime` / `datetime2` / `smalldatetime`
    DateTime(NaiveDateTime),
    /// `datetimeoffset`
    DateTimeOffset(DateTime<FixedOffset>),
    /// SQL `NULL`.
    #[default]
    Null,
}

impl DataValue {
    /// Whether this value is SQL `NULL`.
    pub fn is_null(&self) -> bool {
        matches!(self, DataValue::Null)
    }

    /// The name of the variant, used in conversion error messages.
    pub fn type_name(&self) -> &'static str {
        match self {
            DataValue::TinyInt(_) => "TinyInt",
            DataValue::SmallInt(_) => "SmallInt",
            DataValue::Int(_) => "Int",
            DataValue::BigInt(_) => "BigInt",
            DataValue::Float(_) => "Float",
            DataValue::Decimal(_) => "Decimal",
            DataValue::Bool(_) => "Bool",
            DataValue::Text(_) => "Text",
            DataValue::Binary(_) => "Binary",
            DataValue::Guid(_) => "Guid",
            DataValue::Date(_) => "Date",
            DataValue::Time(_) => "Time",
            DataValue::DateTime(_) => "DateTime",
            DataValue::DateTimeOffset(_) => "DateTimeOffset",
            DataValue::Null => "Null",
        }
    }
}

/// The SQL Server type of a column, as reported by the server.
///
/// This mirrors the TDS type tags. It replaces the previous stringly-typed
/// `sql_type: String`, which exposed the driver's `Debug` output as a public
/// contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
#[allow(missing_docs)]
pub enum SqlType {
    Null,
    Bit,
    TinyInt,
    SmallInt,
    Int,
    BigInt,
    SmallDateTime,
    Real,
    Float,
    Money,
    DateTime,
    SmallMoney,
    Guid,
    IntN,
    BitN,
    Decimal,
    Numeric,
    FloatN,
    DateTimeN,
    Date,
    Time,
    DateTime2,
    DateTimeOffset,
    VarBinary,
    VarChar,
    Binary,
    Char,
    NVarChar,
    NChar,
    Xml,
    Udt,
    Text,
    Image,
    NText,
    Variant,
}

impl From<tiberius::ColumnType> for SqlType {
    fn from(t: tiberius::ColumnType) -> Self {
        use tiberius::ColumnType as C;
        match t {
            C::Null => SqlType::Null,
            C::Bit => SqlType::Bit,
            C::Int1 => SqlType::TinyInt,
            C::Int2 => SqlType::SmallInt,
            C::Int4 => SqlType::Int,
            C::Int8 => SqlType::BigInt,
            C::Datetime4 => SqlType::SmallDateTime,
            C::Float4 => SqlType::Real,
            C::Float8 => SqlType::Float,
            C::Money => SqlType::Money,
            C::Datetime => SqlType::DateTime,
            C::Money4 => SqlType::SmallMoney,
            C::Guid => SqlType::Guid,
            C::Intn => SqlType::IntN,
            C::Bitn => SqlType::BitN,
            C::Decimaln => SqlType::Decimal,
            C::Numericn => SqlType::Numeric,
            C::Floatn => SqlType::FloatN,
            C::Datetimen => SqlType::DateTimeN,
            C::Daten => SqlType::Date,
            C::Timen => SqlType::Time,
            C::Datetime2 => SqlType::DateTime2,
            C::DatetimeOffsetn => SqlType::DateTimeOffset,
            C::BigVarBin => SqlType::VarBinary,
            C::BigVarChar => SqlType::VarChar,
            C::BigBinary => SqlType::Binary,
            C::BigChar => SqlType::Char,
            C::NVarchar => SqlType::NVarChar,
            C::NChar => SqlType::NChar,
            C::Xml => SqlType::Xml,
            C::Udt => SqlType::Udt,
            C::Text => SqlType::Text,
            C::Image => SqlType::Image,
            C::NText => SqlType::NText,
            C::SSVariant => SqlType::Variant,
        }
    }
}

// ---------------------------------------------------------------------------
// Rust -> SQL
// ---------------------------------------------------------------------------

/// A Rust value that can be bound as a SQL parameter.
///
/// Implemented for the primitive types, `chrono` date/time types, [`Decimal`],
/// [`Uuid`], and `Option<T>` for any `T: ToSql` (binding SQL `NULL`).
pub trait ToSql: Send + Sync {
    /// Convert to the dynamic value that gets sent to the server.
    fn to_value(&self) -> DataValue;
}

impl ToSql for DataValue {
    fn to_value(&self) -> DataValue {
        self.clone()
    }
}

impl<T: ToSql + ?Sized> ToSql for &T {
    fn to_value(&self) -> DataValue {
        (**self).to_value()
    }
}

impl<T: ToSql> ToSql for Option<T> {
    fn to_value(&self) -> DataValue {
        match self {
            Some(v) => v.to_value(),
            None => DataValue::Null,
        }
    }
}

macro_rules! impl_to_sql {
    ($($t:ty => $variant:ident),* $(,)?) => {
        $(impl ToSql for $t {
            fn to_value(&self) -> DataValue { DataValue::$variant(self.clone()) }
        })*
    };
}

impl_to_sql! {
    u8 => TinyInt,
    i16 => SmallInt,
    i32 => Int,
    i64 => BigInt,
    f64 => Float,
    bool => Bool,
    Decimal => Decimal,
    String => Text,
    Vec<u8> => Binary,
    Uuid => Guid,
    NaiveDate => Date,
    NaiveTime => Time,
    NaiveDateTime => DateTime,
    DateTime<FixedOffset> => DateTimeOffset,
}

impl ToSql for str {
    fn to_value(&self) -> DataValue {
        DataValue::Text(self.to_string())
    }
}

impl ToSql for [u8] {
    fn to_value(&self) -> DataValue {
        DataValue::Binary(self.to_vec())
    }
}

impl ToSql for f32 {
    fn to_value(&self) -> DataValue {
        DataValue::Float(*self as f64)
    }
}

// `From` conversions, used by `Parameter::new` and by tests building values.
macro_rules! impl_from {
    ($($t:ty => $variant:ident),* $(,)?) => {
        $(impl From<$t> for DataValue {
            fn from(v: $t) -> Self { DataValue::$variant(v) }
        })*
    };
}

impl_from! {
    u8 => TinyInt,
    i16 => SmallInt,
    i32 => Int,
    i64 => BigInt,
    f64 => Float,
    bool => Bool,
    Decimal => Decimal,
    String => Text,
    Vec<u8> => Binary,
    Uuid => Guid,
    NaiveDate => Date,
    NaiveTime => Time,
    NaiveDateTime => DateTime,
    DateTime<FixedOffset> => DateTimeOffset,
}

impl From<&str> for DataValue {
    fn from(v: &str) -> Self {
        DataValue::Text(v.to_string())
    }
}

impl From<&[u8]> for DataValue {
    fn from(v: &[u8]) -> Self {
        DataValue::Binary(v.to_vec())
    }
}

impl From<f32> for DataValue {
    fn from(v: f32) -> Self {
        DataValue::Float(v as f64)
    }
}

/// Binding an `Option` sends SQL `NULL` for `None`.
impl<T: Into<DataValue>> From<Option<T>> for DataValue {
    fn from(v: Option<T>) -> Self {
        match v {
            Some(v) => v.into(),
            None => DataValue::Null,
        }
    }
}

// ---------------------------------------------------------------------------
// SQL -> Rust
// ---------------------------------------------------------------------------

/// A Rust type that can be produced from a SQL value.
///
/// This is the counterpart to [`ToSql`], and is what makes
/// [`Row::get`](crate::Row::get) typed.
pub trait FromSql: Sized {
    /// Convert from the dynamic value, or fail with [`Error::Conversion`].
    ///
    /// `column` names the column being read, purely for the error message.
    fn from_sql(value: &DataValue, column: &str) -> Result<Self>;
}

fn mismatch<T>(value: &DataValue, column: &str, target: &'static str) -> Result<T> {
    Err(Error::Conversion {
        column: column.to_string(),
        actual: value.type_name(),
        target,
    })
}

/// `NULL` becomes `None`; anything else delegates to `T`.
impl<T: FromSql> FromSql for Option<T> {
    fn from_sql(value: &DataValue, column: &str) -> Result<Self> {
        match value {
            DataValue::Null => Ok(None),
            other => T::from_sql(other, column).map(Some),
        }
    }
}

macro_rules! impl_from_sql {
    ($t:ty, $target:literal, |$v:ident| $body:expr) => {
        impl FromSql for $t {
            fn from_sql(value: &DataValue, column: &str) -> Result<Self> {
                let $v = value;
                match $body {
                    Some(v) => Ok(v),
                    None => mismatch(value, column, $target),
                }
            }
        }
    };
}

// Integers widen upward but never narrow, so a value is never silently truncated.
impl_from_sql!(u8, "u8", |v| match v {
    DataValue::TinyInt(n) => Some(*n),
    _ => None,
});

impl_from_sql!(i16, "i16", |v| match v {
    DataValue::TinyInt(n) => Some(*n as i16),
    DataValue::SmallInt(n) => Some(*n),
    _ => None,
});

impl_from_sql!(i32, "i32", |v| match v {
    DataValue::TinyInt(n) => Some(*n as i32),
    DataValue::SmallInt(n) => Some(*n as i32),
    DataValue::Int(n) => Some(*n),
    _ => None,
});

impl_from_sql!(i64, "i64", |v| match v {
    DataValue::TinyInt(n) => Some(*n as i64),
    DataValue::SmallInt(n) => Some(*n as i64),
    DataValue::Int(n) => Some(*n as i64),
    DataValue::BigInt(n) => Some(*n),
    _ => None,
});

impl_from_sql!(f64, "f64", |v| match v {
    DataValue::Float(n) => Some(*n),
    _ => None,
});

impl_from_sql!(bool, "bool", |v| match v {
    DataValue::Bool(b) => Some(*b),
    _ => None,
});

impl_from_sql!(Decimal, "Decimal", |v| match v {
    DataValue::Decimal(d) => Some(*d),
    _ => None,
});

impl_from_sql!(String, "String", |v| match v {
    DataValue::Text(s) => Some(s.clone()),
    _ => None,
});

impl_from_sql!(Vec<u8>, "Vec<u8>", |v| match v {
    DataValue::Binary(b) => Some(b.clone()),
    _ => None,
});

impl_from_sql!(Uuid, "Uuid", |v| match v {
    DataValue::Guid(g) => Some(*g),
    _ => None,
});

impl_from_sql!(NaiveDate, "NaiveDate", |v| match v {
    DataValue::Date(d) => Some(*d),
    _ => None,
});

impl_from_sql!(NaiveTime, "NaiveTime", |v| match v {
    DataValue::Time(t) => Some(*t),
    _ => None,
});

impl_from_sql!(NaiveDateTime, "NaiveDateTime", |v| match v {
    DataValue::DateTime(d) => Some(*d),
    _ => None,
});

impl_from_sql!(
    DateTime<FixedOffset>,
    "DateTime<FixedOffset>",
    |v| match v {
        DataValue::DateTimeOffset(d) => Some(*d),
        _ => None,
    }
);

impl FromSql for DataValue {
    fn from_sql(value: &DataValue, _column: &str) -> Result<Self> {
        Ok(value.clone())
    }
}

// ---------------------------------------------------------------------------
// Comparisons against native types, so assertions stay terse.
// ---------------------------------------------------------------------------

macro_rules! impl_partial_eq {
    ($t:ty, |$s:ident, $o:ident| $body:expr) => {
        impl PartialEq<$t> for DataValue {
            fn eq(&self, other: &$t) -> bool {
                let $s = self;
                let $o = other;
                $body
            }
        }
    };
}

// Integer comparisons widen so `assert_eq!(row["c"], 1)` works whatever the
// server actually sent.
impl_partial_eq!(i64, |s, o| match s {
    DataValue::TinyInt(v) => *v as i64 == *o,
    DataValue::SmallInt(v) => *v as i64 == *o,
    DataValue::Int(v) => *v as i64 == *o,
    DataValue::BigInt(v) => *v == *o,
    _ => false,
});
impl_partial_eq!(i32, |s, o| *s == (*o as i64));
impl_partial_eq!(i16, |s, o| *s == (*o as i64));
impl_partial_eq!(u8, |s, o| *s == (*o as i64));
impl_partial_eq!(bool, |s, o| matches!(s, DataValue::Bool(v) if v == o));
impl_partial_eq!(f64, |s, o| matches!(s, DataValue::Float(v) if v == o));
impl_partial_eq!(Decimal, |s, o| matches!(s, DataValue::Decimal(v) if v == o));
impl_partial_eq!(
    str,
    |s, o| matches!(s, DataValue::Text(v) if v.as_str() == o)
);
impl_partial_eq!(
    &str,
    |s, o| matches!(s, DataValue::Text(v) if v.as_str() == *o)
);
impl_partial_eq!(String, |s, o| matches!(s, DataValue::Text(v) if v == o));
impl_partial_eq!(Uuid, |s, o| matches!(s, DataValue::Guid(v) if v == o));
impl_partial_eq!(NaiveDate, |s, o| matches!(s, DataValue::Date(v) if v == o));
impl_partial_eq!(NaiveTime, |s, o| matches!(s, DataValue::Time(v) if v == o));
impl_partial_eq!(
    NaiveDateTime,
    |s, o| matches!(s, DataValue::DateTime(v) if v == o)
);
impl_partial_eq!(DateTime<FixedOffset>, |s, o| {
    matches!(s, DataValue::DateTimeOffset(v) if v == o)
});
impl_partial_eq!(Vec<u8>, |s, o| matches!(s, DataValue::Binary(v) if v == o));
impl_partial_eq!(
    &[u8],
    |s, o| matches!(s, DataValue::Binary(v) if v.as_slice() == *o)
);

#[cfg(test)]
mod tests;
