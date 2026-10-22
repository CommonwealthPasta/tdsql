//! Connection set-up: the connect timeout and named instances.
//!
//! The `#[ignore]`d tests need SQL Server on localhost:1433:
//!
//! ```text
//! docker run -e ACCEPT_EULA=Y -e SA_PASSWORD='YourStrong!Passw0rd' \
//!     -p 1433:1433 -d mcr.microsoft.com/mssql/server:2022-latest
//! cargo test --test connect_test -- --include-ignored
//! ```

use std::time::{Duration, Instant};

use tdsql::{Client, Config, Error};
use tokio::net::{TcpListener, UdpSocket};

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

/// Answers SQL Server Browser's instance lookup (MC-SQLR 2.2.2) on UDP 1434
/// with TCP port 1433. `None` if something else holds the port.
async fn fake_browser(instance: &'static str) -> Option<tokio::task::JoinHandle<()>> {
    let socket = UdpSocket::bind("127.0.0.1:1434").await.ok()?;
    Some(tokio::spawn(async move {
        let mut buf = [0u8; 512];
        let (len, from) = socket.recv_from(&mut buf).await.unwrap();
        // CLNT_UCAST_INST: 0x04, then the instance name.
        assert_eq!(&buf[..len], [&[4u8], instance.as_bytes()].concat());
        let data = format!(
            "ServerName;FAKE;InstanceName;{instance};IsClustered;No;Version;16.0.1000.6;tcp;1433;;"
        );
        let mut reply = vec![5u8];
        reply.extend_from_slice(&(data.len() as u16).to_le_bytes());
        reply.extend_from_slice(data.as_bytes());
        socket.send_to(&reply, from).await.unwrap();
    }))
}

#[tokio::test]
#[ignore]
async fn a_named_instance_is_found_through_sql_server_browser() {
    let Some(browser) = fake_browser("MSSQLSERVER").await else {
        eprintln!("skipped: UDP 1434 is in use");
        return;
    };

    // Before, this connected over TCP to 1434, the Browser's port. The server
    // checks the name too, so it is the container's own.
    let config = Config::from_ado_string(format!("Server=127.0.0.1\\MSSQLSERVER;{LOGIN}"));
    let mut client = Client::connect(&config).await.unwrap();
    let one: i32 = client.query_scalar("SELECT 1", &[]).await.unwrap();
    assert_eq!(one, 1);
    browser.await.unwrap();
}
