//! Connection set-up: the connect timeout, named instances and redirects.
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
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};

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

fn packet(ty: u8, payload: &[u8]) -> Vec<u8> {
    let len = (payload.len() + 8) as u16;
    let mut p = vec![ty, 1, (len >> 8) as u8, len as u8, 0, 0, 1, 0];
    p.extend_from_slice(payload);
    p
}

/// Reads one message from the client.
async fn read_message(s: &mut TcpStream) {
    loop {
        let mut header = [0u8; 8];
        s.read_exact(&mut header).await.unwrap();
        let mut body = vec![0u8; u16::from_be_bytes([header[2], header[3]]) as usize - 8];
        s.read_exact(&mut body).await.unwrap();
        if header[1] & 1 == 1 {
            return;
        }
    }
}

/// A gateway that answers the login with a redirect to `host:port`, as Azure
/// SQL's does.
async fn redirecting_gateway(host: &'static str, port: u16) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gateway = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();

        // PRELOGIN: version, then ENCRYPT_NOT_SUP.
        read_message(&mut s).await;
        let prelogin = [
            0x00, 0, 11, 0, 6, 0x01, 0, 17, 0, 1, 0xff, 16, 0, 0x07, 0xd0, 0, 0, 2,
        ];
        s.write_all(&packet(4, &prelogin)).await.unwrap();

        // LOGIN7, answered with ENVCHANGE routing (MS-TDS 2.2.7.9) and DONE.
        read_message(&mut s).await;
        let host: Vec<u8> = host.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let mut value = vec![0u8];
        value.extend_from_slice(&port.to_le_bytes());
        value.extend_from_slice(&((host.len() / 2) as u16).to_le_bytes());
        value.extend_from_slice(&host);
        let mut change = vec![20u8];
        change.extend_from_slice(&(value.len() as u16).to_le_bytes());
        change.extend_from_slice(&value);
        change.extend_from_slice(&[0, 0]);
        let mut tokens = vec![0xE3];
        tokens.extend_from_slice(&(change.len() as u16).to_le_bytes());
        tokens.extend_from_slice(&change);
        tokens.extend_from_slice(&[0xFD, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        s.write_all(&packet(4, &tokens)).await.unwrap();
        let _ = s.read(&mut [0u8; 1]).await;
    });
    gateway
}

#[tokio::test]
#[ignore]
async fn a_redirect_is_followed() {
    let gateway = redirecting_gateway("127.0.0.1", 1433).await;

    // Unencrypted, as the fake gateway has no TLS.
    let config = Config::from_ado_string(format!(
        "Server=tcp:127.0.0.1,{gateway};{LOGIN};Encrypt=DANGER_PLAINTEXT"
    ));
    // Before, this failed with the redirect as its error.
    let mut client = Client::connect(&config).await.unwrap();
    let one: i32 = client.query_scalar("SELECT 1", &[]).await.unwrap();
    assert_eq!(one, 1);
}
