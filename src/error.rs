//! Error types for `tdsql`.

/// Errors returned by this crate.
///
/// Every variant carries enough context to act on the failure without
/// string-matching a message. The enum is `#[non_exhaustive]`, so new variants
/// can be added without a breaking release.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The TCP connection or TDS login failed.
    #[error("failed to connect to {server}")]
    Connect {
        /// The `host:port` that was dialed.
        server: String,
        /// The underlying driver failure.
        #[source]
        source: Box<tiberius::error::Error>,
    },

    /// A network-level I/O failure.
    #[error("network I/O error")]
    Io(#[from] std::io::Error),

    /// The server rejected the statement, or the connection failed mid-stream.
    #[error("query failed")]
    Query(#[source] Box<tiberius::error::Error>),

    /// No column with this name exists in the row.
    #[error("column '{0}' not found")]
    ColumnNotFound(String),

    /// A positional column lookup was out of range.
    #[error("column index {index} out of range ({len} columns)")]
    ColumnIndexOutOfRange {
        /// The requested index.
        index: usize,
        /// The number of columns actually present.
        len: usize,
    },

    /// `query_one` matched a number of rows other than exactly one.
    #[error("expected exactly one row, found {found}")]
    UnexpectedRowCount {
        /// How many rows the query actually produced.
        found: usize,
    },

    /// A value could not be converted to the requested Rust type.
    #[error("cannot convert column '{column}' from {actual} to {target}")]
    Conversion {
        /// The column the conversion was attempted on.
        column: String,
        /// The `DataValue` variant actually present.
        actual: &'static str,
        /// The Rust type that was requested.
        target: &'static str,
    },

    /// The server sent a numeric the `Decimal` type cannot represent.
    #[error("invalid decimal from server (value {value}, scale {scale})")]
    Decimal {
        /// The raw mantissa.
        value: i128,
        /// The declared scale.
        scale: u8,
    },

    /// The connection configuration was invalid.
    #[error("invalid configuration: {0}")]
    Config(String),

    /// The connection task has shut down, so no further statements can run.
    ///
    /// This means the connection was closed, or it failed in a way that ended
    /// the background task driving it.
    #[error("connection is closed")]
    ConnectionClosed,

    /// A blocking client was created from inside an async runtime.
    ///
    /// The blocking client owns a runtime, and driving one runtime from inside
    /// another panics. Use [`Client`](crate::Client) directly in async code.
    #[error("the blocking client cannot be used from inside an async runtime")]
    BlockingInAsync,

    /// A savepoint name was not a plain SQL identifier.
    ///
    /// Savepoint names are interpolated into `ROLLBACK TRANSACTION <name>`, so
    /// they are restricted to letters, digits and underscores, starting with a
    /// letter or underscore, at most 32 characters.
    #[error("invalid savepoint name: {0:?}")]
    InvalidSavepointName(String),
}

impl From<tiberius::error::Error> for Error {
    fn from(e: tiberius::error::Error) -> Self {
        Error::Query(Box::new(e))
    }
}

/// A `Result` alias using this crate's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    // The public error type must stay `Send + Sync + 'static` so downstream
    // `anyhow::Result` callers keep absorbing it with `?`.
    fn assert_traits<T: std::error::Error + Send + Sync + 'static>() {}

    #[test]
    fn error_is_send_sync_static() {
        assert_traits::<Error>();
    }

    #[test]
    fn messages_carry_context() {
        let e = Error::ColumnNotFound("id".into());
        assert_eq!(e.to_string(), "column 'id' not found");

        let e = Error::UnexpectedRowCount { found: 3 };
        assert_eq!(e.to_string(), "expected exactly one row, found 3");

        let e = Error::Conversion {
            column: "amount".into(),
            actual: "Text",
            target: "i32",
        };
        assert_eq!(
            e.to_string(),
            "cannot convert column 'amount' from Text to i32"
        );
    }
}
