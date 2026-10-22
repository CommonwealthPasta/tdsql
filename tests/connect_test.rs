//! Connection set-up: the connect timeout.

use std::time::{Duration, Instant};

use tdsql::{Client, Config, Error};
use tokio::net::TcpListener;

const LOGIN: &str = "User Id=sa;Password=YourStrong!Passw0rd;TrustServerCertificate=true";

/// A server that accepts connections and never answers.
async fn silent_server() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((socket, _)) = listener.accept().await {
            held.push(socket);
        }
    });
    port
}

async fn assert_times_out(config: Config, limit: Duration) {
    let started = Instant::now();
    let err = Client::connect(&config).await.unwrap_err();
    let elapsed = started.elapsed();

    match &err {
        Error::Connect { source, .. } => assert!(
            matches!(
                **source,
                tiberius::error::Error::Io {
                    kind: std::io::ErrorKind::TimedOut,
                    ..
                }
            ),
            "got {err}"
        ),
        other => panic!("expected a connect error, got {other}"),
    }
    assert!(
        elapsed >= limit && elapsed < limit + Duration::from_secs(2),
        "{elapsed:?}"
    );
}

#[tokio::test]
async fn a_server_that_never_answers_times_out() {
    let port = silent_server().await;

    // Before, connect waited forever.
    let ado = format!("Server=tcp:127.0.0.1,{port};{LOGIN};Connect Timeout=1");
    assert_times_out(Config::from_ado_string(ado), Duration::from_secs(1)).await;

    let parts = Config::new()
        .host("127.0.0.1")
        .port(port)
        .auth("sa", "x")
        .connect_timeout(Some(Duration::from_millis(500)));
    assert_times_out(parts, Duration::from_millis(500)).await;
}
