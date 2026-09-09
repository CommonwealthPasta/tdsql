//! Error types for `tdsql`.

use std::fmt;

/// How a failing statement was sent to the server.
///
/// Carried by [`Error::Query`], so a log line says whether the failure came
/// from a parameterised statement, a verbatim batch, a stored procedure, or the
/// transaction control this crate issues on the caller's behalf.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum StatementKind {
    /// A parameterised statement, sent as an RPC.
    Query,
    /// A SQL batch, sent verbatim.
    Batch,
    /// A stored procedure, invoked by name.
    StoredProcedure,
    /// `BEGIN` / `COMMIT` / `ROLLBACK` / `SAVE TRANSACTION`.
    TransactionControl,
}

impl fmt::Display for StatementKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            StatementKind::Query => "query",
            StatementKind::Batch => "batch",
            StatementKind::StoredProcedure => "stored procedure",
            StatementKind::TransactionControl => "transaction control statement",
        })
    }
}

/// The statement, on one line and truncated, for an error message.
///
/// Renders as ` [SELECT ...]`, or as nothing at all when the statement is not
/// known, so it can be appended unconditionally.
struct Statement<'a>(&'a str);

impl Statement<'_> {
    /// How much of the statement the message shows. The full text stays
    /// reachable through [`Error::statement`].
    const MAX_CHARS: usize = 200;
}

impl fmt::Display for Statement<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // A batch is usually multi-line and indented, and an error message that
        // spans lines is unreadable in a log, so collapse the whitespace.
        let text = self.0.split_whitespace().collect::<Vec<_>>().join(" ");
        if text.is_empty() {
            return Ok(());
        }
        if text.chars().count() > Self::MAX_CHARS {
            let head: String = text.chars().take(Self::MAX_CHARS).collect();
            write!(f, " [{head}...]")
        } else {
            write!(f, " [{text}]")
        }
    }
}

/// The driver failure, with the server's own diagnostics spelled out.
///
/// The driver error is the `source` as well, but a caller who prints only the
/// top-level message would otherwise never see the one thing that explains the
/// failure: what the server actually said.
struct Detail<'a>(&'a tiberius::error::Error);

impl fmt::Display for Detail<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            tiberius::error::Error::Server(token) => {
                write!(f, ": {}", token.message())?;
                if !token.procedure().is_empty() {
                    write!(f, " (in {})", token.procedure())?;
                }
                write!(
                    f,
                    " [error {}, state {}, severity {}, line {}]",
                    token.code(),
                    token.state(),
                    token.class(),
                    token.line()
                )
            }
            other => write!(f, ": {other}"),
        }
    }
}

/// Errors returned by this crate.
///
/// Every variant carries enough context to act on the failure without
/// string-matching a message. The enum is `#[non_exhaustive]`, so new variants
/// can be added without a breaking release.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The TCP connection or TDS login failed.
    #[error("failed to connect to {server}{}", Detail(.source))]
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
    ///
    /// The message names the statement that was running and, when the server
    /// reported one, its error number, state, severity and line, so a bare
    /// `{e}` is enough to act on:
    ///
    /// ```text
    /// query failed [SELECT id FROM nope]: Invalid object name 'nope'. [error 208, state 1, severity 16, line 1]
    /// ```
    ///
    /// Parameter *values* are deliberately not captured: they routinely hold
    /// personal data that has no business in a log. Only how many were bound is
    /// recorded.
    #[error("{kind} failed{}{}", Statement(.statement), Detail(.source))]
    #[non_exhaustive]
    Query {
        /// How the statement was sent.
        kind: StatementKind,
        /// The SQL text, or the stored procedure name. Empty if not known.
        statement: String,
        /// How many parameters were bound.
        parameters: usize,
        /// The underlying driver failure.
        #[source]
        source: Box<tiberius::error::Error>,
    },

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

impl Error {
    /// Name the statement a driver failure came from.
    ///
    /// A failure raised while draining a result stream reaches us through the
    /// blanket `From` impl, which has no idea what was running; the connection
    /// task does know, and fills it in on the way out. Only a [`Error::Query`]
    /// still missing its context is rewritten, so an error that already names a
    /// statement, or an error of any other kind, passes through untouched.
    pub(crate) fn with_statement(
        self,
        kind: StatementKind,
        statement: &str,
        parameters: usize,
    ) -> Self {
        match self {
            Error::Query {
                statement: known,
                source,
                ..
            } if known.is_empty() => Error::Query {
                kind,
                statement: statement.to_string(),
                parameters,
                source,
            },
            other => other,
        }
    }

    /// The statement that was running, when this crate knows what it was.
    ///
    /// The SQL text for a query or a batch; the procedure name for a stored
    /// procedure call.
    pub fn statement(&self) -> Option<&str> {
        match self {
            Error::Query { statement, .. } if !statement.is_empty() => Some(statement),
            _ => None,
        }
    }

    /// How the failing statement was sent.
    pub fn statement_kind(&self) -> Option<StatementKind> {
        match self {
            Error::Query { kind, .. } => Some(*kind),
            _ => None,
        }
    }

    /// How many parameters were bound to the failing statement.
    pub fn parameter_count(&self) -> Option<usize> {
        match self {
            Error::Query { parameters, .. } => Some(*parameters),
            _ => None,
        }
    }

    /// The server's own error token, when the failure came from the server
    /// rather than from the socket or the protocol layer.
    fn token(&self) -> Option<&tiberius::error::TokenError> {
        let source = match self {
            Error::Connect { source, .. } | Error::Query { source, .. } => source.as_ref(),
            _ => return None,
        };
        match source {
            tiberius::error::Error::Server(token) => Some(token),
            _ => None,
        }
    }

    /// The SQL Server error number, e.g. `208` for an invalid object name.
    ///
    /// `None` when the failure never reached the server: a socket error, a TLS
    /// failure, a protocol mismatch.
    ///
    /// ```
    /// # fn handle(err: tdsql::Error) {
    /// match err.code() {
    ///     Some(2627) => { /* duplicate key: the row is already there */ }
    ///     Some(1205) => { /* deadlock victim: retry the transaction */ }
    ///     _ => {}
    /// }
    /// # }
    /// ```
    pub fn code(&self) -> Option<u32> {
        self.token().map(|t| t.code())
    }

    /// The message text the server sent, without this crate's framing.
    pub fn server_message(&self) -> Option<&str> {
        self.token().map(|t| t.message())
    }

    /// The severity the server assigned. Under 10 is informational, 11-16 is a
    /// user-correctable error, 17 and up is a server-side problem.
    pub fn severity(&self) -> Option<u8> {
        self.token().map(|t| t.class())
    }

    /// The error state, which distinguishes the places one error number can be
    /// raised from.
    pub fn state(&self) -> Option<u8> {
        self.token().map(|t| t.state())
    }

    /// The 1-based line of the batch or procedure that failed. `0` means the
    /// server did not attribute the error to a line.
    pub fn line(&self) -> Option<u32> {
        self.token().map(|t| t.line())
    }

    /// The stored procedure the server was executing, if it was in one.
    pub fn procedure(&self) -> Option<&str> {
        self.token()
            .map(|t| t.procedure())
            .filter(|p| !p.is_empty())
    }

    /// The name the server reported for itself.
    pub fn server_name(&self) -> Option<&str> {
        self.token().map(|t| t.server()).filter(|s| !s.is_empty())
    }

    /// Whether the statement was picked as a deadlock victim (error `1205`),
    /// which is the signal to retry the transaction.
    pub fn is_deadlock(&self) -> bool {
        self.code() == Some(1205)
    }
}

impl From<tiberius::error::Error> for Error {
    /// Wrap a driver failure that carries no statement context.
    ///
    /// The connection task fills the context in with `with_statement` before
    /// the error reaches a caller.
    fn from(e: tiberius::error::Error) -> Self {
        Error::Query {
            kind: StatementKind::Query,
            statement: String::new(),
            parameters: 0,
            source: Box::new(e),
        }
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

    // A `Server` token cannot be built outside the driver, so these cover the
    // context this crate adds; the server's own fields are read through
    // `token()`, which every accessor shares.
    fn driver_error() -> tiberius::error::Error {
        tiberius::error::Error::Protocol("stream desync".into())
    }

    fn query_error(kind: StatementKind, statement: &str, parameters: usize) -> Error {
        Error::from(driver_error()).with_statement(kind, statement, parameters)
    }

    #[test]
    fn query_error_names_the_statement() {
        let e = query_error(
            StatementKind::Query,
            "SELECT id FROM users WHERE id > @P1",
            1,
        );
        assert_eq!(
            e.to_string(),
            "query failed [SELECT id FROM users WHERE id > @P1]: Protocol error: stream desync"
        );
        assert_eq!(e.statement(), Some("SELECT id FROM users WHERE id > @P1"));
        assert_eq!(e.statement_kind(), Some(StatementKind::Query));
        assert_eq!(e.parameter_count(), Some(1));
    }

    #[test]
    fn statement_kind_names_the_route() {
        let e = query_error(StatementKind::StoredProcedure, "dbo.sp_upsert_order", 2);
        assert!(e
            .to_string()
            .starts_with("stored procedure failed [dbo.sp_upsert_order]"));

        let e = query_error(StatementKind::Batch, "CREATE VIEW v AS SELECT 1 AS x", 0);
        assert!(e.to_string().starts_with("batch failed ["));

        let e = query_error(StatementKind::TransactionControl, "COMMIT TRANSACTION", 0);
        assert!(e
            .to_string()
            .starts_with("transaction control statement failed ["));
    }

    #[test]
    fn multi_line_statements_collapse_onto_one_line() {
        let e = query_error(StatementKind::Batch, "SELECT 1\n  FROM  dual\n", 0);
        assert_eq!(
            e.to_string(),
            "batch failed [SELECT 1 FROM dual]: Protocol error: stream desync"
        );
    }

    #[test]
    fn long_statements_are_truncated_in_the_message() {
        let long = format!("SELECT {}", "x".repeat(500));
        let e = query_error(StatementKind::Query, &long, 0);

        let message = e.to_string();
        assert!(message.contains("..."), "{message}");
        // The message is bounded, but the full text stays reachable.
        assert!(message.len() < long.len(), "{message}");
        assert_eq!(e.statement(), Some(long.as_str()));
    }

    #[test]
    fn context_is_only_filled_in_once() {
        let e = query_error(StatementKind::Query, "SELECT 1", 0).with_statement(
            StatementKind::Batch,
            "SELECT 2",
            7,
        );
        assert_eq!(e.statement(), Some("SELECT 1"));
        assert_eq!(e.parameter_count(), Some(0));

        // Errors of other kinds pass through untouched.
        let e = Error::UnexpectedRowCount { found: 3 }.with_statement(
            StatementKind::Query,
            "SELECT 1",
            0,
        );
        assert!(matches!(e, Error::UnexpectedRowCount { found: 3 }));
    }

    #[test]
    fn server_fields_are_absent_when_the_server_never_answered() {
        let e = query_error(StatementKind::Query, "SELECT 1", 0);
        assert_eq!(e.code(), None);
        assert_eq!(e.server_message(), None);
        assert_eq!(e.severity(), None);
        assert_eq!(e.state(), None);
        assert_eq!(e.line(), None);
        assert_eq!(e.procedure(), None);
        assert_eq!(e.server_name(), None);
        assert!(!e.is_deadlock());

        // Nor for an error that never involved a server at all.
        let e = Error::ConnectionClosed;
        assert_eq!(e.code(), None);
        assert_eq!(e.statement(), None);
        assert_eq!(e.statement_kind(), None);
        assert_eq!(e.parameter_count(), None);
    }

    #[test]
    fn connect_errors_carry_the_driver_detail() {
        let e = Error::Connect {
            server: "localhost:1433".into(),
            source: Box::new(driver_error()),
        };
        assert_eq!(
            e.to_string(),
            "failed to connect to localhost:1433: Protocol error: stream desync"
        );
    }
}
