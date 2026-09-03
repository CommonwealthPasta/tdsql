//! Connection configuration.

use crate::error::{Error, Result};

#[derive(Clone, PartialEq, Eq)]
enum Source {
    Parts {
        host: String,
        port: u16,
        database: Option<String>,
        username: String,
        password: String,
        trust_cert: bool,
        application_name: Option<String>,
    },
    /// An ADO.NET-style connection string, parsed by the driver on connect.
    Ado(String),
    /// A JDBC-style connection string, parsed by the driver on connect.
    Jdbc(String),
}

/// How to reach a SQL Server instance.
///
/// Build it fluently:
///
/// ```
/// use tdsql::Config;
///
/// let config = Config::new()
///     .host("localhost")
///     .port(1433)
///     .database("master")
///     .auth("sa", "YourStrong!Passw0rd")
///     .trust_cert();
/// ```
///
/// or parse a connection string:
///
/// ```
/// use tdsql::Config;
///
/// let config = Config::from_ado_string(
///     "Server=tcp:localhost,1433;Database=master;User Id=sa;Password=pw;TrustServerCertificate=true",
/// );
/// ```
#[derive(Clone, PartialEq, Eq)]
pub struct Config {
    source: Source,
}

impl Default for Config {
    fn default() -> Self {
        Self::new()
    }
}

impl Config {
    /// Start a new configuration, defaulting to `localhost:1433`.
    pub fn new() -> Self {
        Self {
            source: Source::Parts {
                host: "localhost".to_string(),
                port: 1433,
                database: None,
                username: String::new(),
                password: String::new(),
                trust_cert: false,
                application_name: None,
            },
        }
    }

    /// Configure from an ADO.NET-style connection string.
    ///
    /// The string is parsed by the driver when connecting, so errors surface
    /// from [`Client::connect`](crate::Client::connect).
    pub fn from_ado_string(s: impl Into<String>) -> Self {
        Self {
            source: Source::Ado(s.into()),
        }
    }

    /// Configure from a JDBC-style connection string.
    pub fn from_jdbc_string(s: impl Into<String>) -> Self {
        Self {
            source: Source::Jdbc(s.into()),
        }
    }

    /// Set the server hostname.
    pub fn host(mut self, host: impl Into<String>) -> Self {
        if let Source::Parts { host: h, .. } = &mut self.source {
            *h = host.into();
        }
        self
    }

    /// Set the TCP port.
    pub fn port(mut self, port: u16) -> Self {
        if let Source::Parts { port: p, .. } = &mut self.source {
            *p = port;
        }
        self
    }

    /// Set the initial database.
    pub fn database(mut self, database: impl Into<String>) -> Self {
        if let Source::Parts { database: d, .. } = &mut self.source {
            *d = Some(database.into());
        }
        self
    }

    /// Use SQL Server authentication with these credentials.
    pub fn auth(mut self, username: impl Into<String>, password: impl Into<String>) -> Self {
        if let Source::Parts {
            username: u,
            password: p,
            ..
        } = &mut self.source
        {
            *u = username.into();
            *p = password.into();
        }
        self
    }

    /// Accept the server's TLS certificate without validating it.
    ///
    /// Convenient against a local container; do not use in production.
    pub fn trust_cert(mut self) -> Self {
        if let Source::Parts { trust_cert: t, .. } = &mut self.source {
            *t = true;
        }
        self
    }

    /// Set the application name reported to the server.
    pub fn application_name(mut self, name: impl Into<String>) -> Self {
        if let Source::Parts {
            application_name: a,
            ..
        } = &mut self.source
        {
            *a = Some(name.into());
        }
        self
    }

    /// Build the driver configuration.
    pub(crate) fn to_tiberius(&self) -> Result<tiberius::Config> {
        match &self.source {
            Source::Ado(s) => tiberius::Config::from_ado_string(s)
                .map_err(|e| Error::Config(format!("invalid ADO connection string: {e}"))),
            Source::Jdbc(s) => tiberius::Config::from_jdbc_string(s)
                .map_err(|e| Error::Config(format!("invalid JDBC connection string: {e}"))),
            Source::Parts {
                host,
                port,
                database,
                username,
                password,
                trust_cert,
                application_name,
            } => {
                let mut cfg = tiberius::Config::new();
                cfg.host(host);
                cfg.port(*port);
                if let Some(db) = database {
                    cfg.database(db);
                }
                cfg.authentication(tiberius::AuthMethod::sql_server(username, password));
                if *trust_cert {
                    cfg.trust_cert();
                }
                if let Some(name) = application_name {
                    cfg.application_name(name);
                }
                Ok(cfg)
            }
        }
    }
}

// Hand-written so the password never lands in a log line.
impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.source {
            Source::Ado(_) => f
                .debug_struct("Config")
                .field("source", &"ado_string(<redacted>)")
                .finish(),
            Source::Jdbc(_) => f
                .debug_struct("Config")
                .field("source", &"jdbc_string(<redacted>)")
                .finish(),
            Source::Parts {
                host,
                port,
                database,
                username,
                trust_cert,
                application_name,
                ..
            } => f
                .debug_struct("Config")
                .field("host", host)
                .field("port", port)
                .field("database", database)
                .field("username", username)
                .field("password", &"<redacted>")
                .field("trust_cert", trust_cert)
                .field("application_name", application_name)
                .finish(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_sets_fields() {
        let cfg = Config::new()
            .host("db.example.com")
            .port(1444)
            .database("sales")
            .auth("sa", "hunter2")
            .trust_cert();

        let Source::Parts {
            host,
            port,
            database,
            username,
            trust_cert,
            ..
        } = &cfg.source
        else {
            panic!("expected parts");
        };
        assert_eq!(host, "db.example.com");
        assert_eq!(*port, 1444);
        assert_eq!(database.as_deref(), Some("sales"));
        assert_eq!(username, "sa");
        assert!(trust_cert);
    }

    #[test]
    fn defaults_to_localhost() {
        let cfg = Config::new();
        let Source::Parts { host, port, .. } = &cfg.source else {
            panic!("expected parts");
        };
        assert_eq!(host, "localhost");
        assert_eq!(*port, 1433);
    }

    #[test]
    fn debug_redacts_password() {
        let cfg = Config::new().auth("sa", "hunter2");
        let rendered = format!("{cfg:?}");
        assert!(!rendered.contains("hunter2"), "password leaked: {rendered}");
        assert!(rendered.contains("<redacted>"));
    }

    #[test]
    fn ado_string_debug_is_redacted() {
        let cfg = Config::from_ado_string("Server=x;Password=hunter2;");
        let rendered = format!("{cfg:?}");
        assert!(!rendered.contains("hunter2"), "password leaked: {rendered}");
    }

    #[test]
    fn builds_tiberius_config_from_parts() {
        let cfg = Config::new()
            .host("localhost")
            .auth("sa", "pw")
            .trust_cert();
        assert!(cfg.to_tiberius().is_ok());
    }

    #[test]
    fn rejects_malformed_ado_string() {
        let cfg = Config::from_ado_string("this is not a connection string");
        assert!(matches!(cfg.to_tiberius(), Err(Error::Config(_))));
    }
}
