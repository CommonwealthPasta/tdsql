#![doc = include_str!("../README.md")]
#![warn(missing_docs)]
#![cfg_attr(docsrs, feature(doc_cfg))]

#[cfg(feature = "blocking")]
#[cfg_attr(docsrs, doc(cfg(feature = "blocking")))]
pub mod blocking;

mod client;
mod command;
mod config;
mod dataset;
mod error;
mod executor;
mod row;
mod stream;
mod transaction;
mod value;

pub use client::Client;
pub use command::{Command, CommandType, Parameter};
pub use config::{Config, DateTimeParameterType};
pub use dataset::{DataSet, DataTable};
pub use error::{Error, Result, StatementKind};
pub use executor::Executor;
pub use row::{Column, Row, RowIndex};
pub use stream::RowStream;
pub use transaction::{IsolationLevel, Transaction};
pub use value::{DataValue, FromSql, SqlType, ToSql};

// The crates whose types appear in this crate's public API are re-exported, so
// callers can reach them at a version that is guaranteed to match. Binding a
// `chrono::DateTime` or a `uuid::Uuid` only type-checks against the exact
// version `tdsql` was built with; a second copy in the dependency graph
// produces the notoriously opaque "expected `Uuid`, found `Uuid`" error.
//
// This follows Effective Rust item 24 ("re-export dependencies whose types
// appear in your API") and matches `sqlx::types`. The trade-off is real and
// deliberate: it couples `tdsql`'s major version to these three, so a breaking
// release of any of them forces one here. All three are mature and moving
// slowly, which is what makes that price acceptable.

/// The `chrono` version this crate binds against -- `tdsql::chrono::Utc`.
///
/// Its types appear in the public API as [`DataValue::Date`],
/// [`DataValue::Time`], [`DataValue::DateTime`] and
/// [`DataValue::DateTimeOffset`].
pub use chrono;

/// The `rust_decimal` version this crate binds against.
///
/// Its [`Decimal`](rust_decimal::Decimal) is the public API's
/// [`DataValue::Decimal`].
pub use rust_decimal;

/// The `uuid` version this crate binds against -- `tdsql::uuid::Uuid`.
///
/// Its [`Uuid`](uuid::Uuid) is the public API's [`DataValue::Guid`], which is
/// how SQL Server's `uniqueidentifier` is read and bound.
pub use uuid;

/// Longer guides, also found in the repository's `docs/` directory.
///
/// Their examples are compiled with the rest of the documentation.
#[cfg(doc)]
pub mod docs {
    #[doc = include_str!("../docs/types.md")]
    pub mod types {}

    #[doc = include_str!("../docs/how-it-works.md")]
    pub mod how_it_works {}
}
