//! What a client reports once the server has ended the session: the cause,
//! with its statement, and from then on that the connection is closed.
//!
//! The tests need SQL Server on localhost:1433:
//!
//! ```text
//! docker run -e ACCEPT_EULA=Y -e SA_PASSWORD='YourStrong!Passw0rd' \
//!     -p 1433:1433 -d mcr.microsoft.com/mssql/server:2022-latest
//! cargo test --test dead_connection_test -- --include-ignored
//! ```

use std::time::Duration;

use tdsql::{Client, Config, Error};

async fn connect() -> Client {
    Client::connect(
        &Config::new()
            .host("localhost")
            .port(1433)
            .database("master")
            .auth("sa", "YourStrong!Passw0rd")
            .trust_cert(),
    )
    .await
    .expect("SQL Server on localhost:1433")
}

/// The first failure names the statement and the I/O cause, once. Every later
/// use is `ConnectionClosed`, and the client says it is not open.
async fn assert_dead(client: &mut Client) {
    let first = client
        .query_scalar::<i32>("SELECT 1", &[])
        .await
        .unwrap_err();
    let Error::Query { source, .. } = &first else {
        panic!("expected the I/O failure with its statement, got {first:?}");
    };
    let tds_driver::error::Error::Io { kind, .. } = &**source else {
        panic!("expected an I/O failure, got {first:?}");
    };
    assert_ne!(*kind, std::io::ErrorKind::NotConnected, "{first}");
    assert!(first.is_connection_lost(), "{first}");
    let text = first.to_string();
    assert!(text.contains("SELECT 1"), "{text}");
    assert!(!text.contains("cancel"), "{text}");
    assert_eq!(
        text.matches("An error occurred during the attempt of performing I/O")
            .count(),
        1,
        "{text}"
    );

    for _ in 0..2 {
        let later = client
            .query_scalar::<i32>("SELECT 1", &[])
            .await
            .unwrap_err();
        assert!(matches!(later, Error::ConnectionClosed), "{later:?}");
        assert!(later.is_connection_lost());
    }

    // The connection task ends on its own once the connection is gone; give
    // it a moment.
    for _ in 0..50 {
        if format!("{client:?}").contains("open: false") {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("still {client:?}");
}

#[tokio::test]
#[ignore]
async fn a_killed_connection_reports_the_cause_then_connection_closed() {
    let mut admin = connect().await;
    let mut client = connect().await;

    let spid: i16 = client.query_scalar("SELECT @@SPID", &[]).await.unwrap();
    admin.execute(&format!("KILL {spid}"), &[]).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_dead(&mut client).await;
}

#[tokio::test]
#[ignore]
async fn a_severity_20_error_closes_the_connection() {
    let mut client = connect().await;

    let err = client
        .execute("RAISERROR('fatal', 20, 1) WITH LOG", &[])
        .await
        .unwrap_err();
    assert_eq!(err.code(), Some(50000), "{err}");
    assert!(
        !err.is_connection_lost(),
        "a server error is not an I/O failure: {err}"
    );

    assert_dead(&mut client).await;
}

/// Kill `client`'s session from another connection.
async fn kill(client: &mut Client) {
    let mut admin = connect().await;
    let spid: i16 = client.query_scalar("SELECT @@SPID", &[]).await.unwrap();
    admin.execute(&format!("KILL {spid}"), &[]).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
}

/// The client closes on its own, before anything else is asked of it, and
/// every later use is `ConnectionClosed`.
async fn assert_closed(client: &mut Client) {
    // The connection task ends once it has sent the failure; give it a moment.
    let mut open = true;
    for _ in 0..50 {
        open = !format!("{client:?}").contains("open: false");
        if !open {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!open, "still {client:?}");

    let later = client
        .query_scalar::<i32>("SELECT 1", &[])
        .await
        .unwrap_err();
    assert!(matches!(later, Error::ConnectionClosed), "{later:?}");
}

#[tokio::test]
#[ignore]
async fn a_stream_on_a_killed_connection_reports_the_cause_then_connection_closed() {
    let mut client = connect().await;
    kill(&mut client).await;

    let err = client.query_stream("SELECT 1", &[]).await.unwrap_err();
    assert!(err.is_connection_lost(), "{err:?}");
    assert!(err.to_string().contains("SELECT 1"), "{err}");

    assert_closed(&mut client).await;
}

#[tokio::test]
#[ignore]
async fn beginning_a_transaction_on_a_killed_connection_reports_the_cause() {
    let mut client = connect().await;
    kill(&mut client).await;

    let err = client.transaction().await.unwrap_err();
    assert!(err.is_connection_lost(), "{err:?}");
    assert!(err.to_string().contains("BEGIN TRANSACTION"), "{err}");

    assert_closed(&mut client).await;
}

#[tokio::test]
#[ignore]
async fn committing_on_a_killed_connection_reports_the_cause() {
    let mut client = connect().await;
    let mut tx = client.transaction().await.unwrap();
    tx.execute("SELECT 1", &[]).await.unwrap();

    // The session is killed with the transaction open; nothing remains of it.
    let mut admin = connect().await;
    let spid: i16 = tx.query_scalar("SELECT @@SPID", &[]).await.unwrap();
    admin.execute(&format!("KILL {spid}"), &[]).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    let err = tx.commit().await.unwrap_err();
    assert!(err.is_connection_lost(), "{err:?}");
    assert!(err.to_string().contains("COMMIT TRANSACTION"), "{err}");

    assert_closed(&mut client).await;
}

#[tokio::test]
#[ignore]
async fn a_live_connection_is_open() {
    let client = connect().await;
    assert!(format!("{client:?}").contains("open: true"));
}
