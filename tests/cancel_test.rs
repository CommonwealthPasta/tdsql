//! Cancellation integration tests against a live SQL Server.
//!
//! A request whose caller stops waiting is cancelled on the server. Most of
//! these give up on a query that would otherwise never finish, then check the
//! connection is free again: before cancellation, the next statement queued
//! behind the abandoned one forever.
//!
//! ```text
//! cargo test --test cancel_test -- --ignored
//! ```

use std::future::Future;
use std::time::Duration;

use futures::FutureExt;
use tdsql::{Client, Command, Config};

fn test_config() -> Config {
    Config::new()
        .host("localhost")
        .port(1433)
        .database("master")
        .auth("sa", "YourStrong!Passw0rd")
        .trust_cert()
}

async fn connect() -> Client {
    Client::connect(&test_config())
        .await
        .expect("failed to connect; is SQL Server running on localhost:1433?")
}

/// Rows from the first moment, and for all practical purposes without end:
/// a three-way cross join of a catalog view with thousands of rows.
const ENDLESS: &str = "SELECT CAST(1 AS INT) AS n \
     FROM sys.all_columns a CROSS JOIN sys.all_columns b CROSS JOIN sys.all_columns c";

/// Long enough for the request to be under way, short enough to keep the
/// suite quick.
const GIVE_UP_AFTER: Duration = Duration::from_millis(200);

/// Comfortably longer than a cancel takes, and shorter than the connection
/// task's own deadline for one, so a cancel the server never acknowledges
/// fails here rather than passing late.
const PROMPTLY: Duration = Duration::from_secs(10);

/// Await `future`, failing if it is still pending after [`PROMPTLY`].
async fn promptly<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(PROMPTLY, future)
        .await
        .expect("the connection is still busy with the abandoned request")
}

/// Await `future` for [`GIVE_UP_AFTER`], asserting it had not finished.
async fn give_up<T>(future: impl Future<Output = T>) {
    let finished = tokio::time::timeout(GIVE_UP_AFTER, future).await.is_ok();
    assert!(
        !finished,
        "the request finished before it could be given up on"
    );
}

/// The connection is free, and what comes next sees its own result rather
/// than anything left over from the cancelled request.
async fn assert_free(client: &mut Client) {
    let n: i32 = promptly(client.query_scalar("SELECT 42", &[]))
        .await
        .unwrap();
    assert_eq!(n, 42);
}

#[tokio::test]
#[ignore]
async fn dropping_a_stream_cancels_its_query() {
    let mut client = connect().await;

    let mut rows = client.query_stream(ENDLESS, &[]).await.unwrap();
    rows.try_next().await.unwrap().unwrap();
    drop(rows);

    assert_free(&mut client).await;
}

#[tokio::test]
#[ignore]
async fn giving_up_on_query_cancels_it() {
    let mut client = connect().await;
    give_up(client.query(ENDLESS, &[])).await;
    assert_free(&mut client).await;
}

#[tokio::test]
#[ignore]
async fn giving_up_on_a_batch_cancels_it() {
    let mut client = connect().await;
    give_up(client.batch(ENDLESS)).await;
    assert_free(&mut client).await;
}

#[tokio::test]
#[ignore]
async fn giving_up_on_a_stored_procedure_cancels_it() {
    let mut client = connect().await;
    client
        .batch(&format!("CREATE PROCEDURE #tdsql_endless AS {ENDLESS}"))
        .await
        .unwrap();
    let endless = Command::stored_procedure("#tdsql_endless");

    give_up(client.query_dataset(&endless)).await;
    assert_free(&mut client).await;

    // And run for its row count.
    give_up(client.execute_command(&endless)).await;
    assert_free(&mut client).await;
}

#[tokio::test]
#[ignore]
async fn cancelling_stops_the_rest_of_the_batch() {
    let mut client = connect().await;
    client
        .batch("CREATE TABLE #tdsql_cancel_after (id INT)")
        .await
        .unwrap();

    let mut rows = client
        .query_stream(
            &format!(
                "INSERT INTO #tdsql_cancel_after (id) VALUES (1); \
                 {ENDLESS}; \
                 INSERT INTO #tdsql_cancel_after (id) VALUES (2)"
            ),
            &[],
        )
        .await
        .unwrap();
    rows.try_next().await.unwrap().unwrap();
    drop(rows);

    // What ran before the cancel stays done; what came after never ran.
    let ids: Vec<i32> = promptly(client.query("SELECT id FROM #tdsql_cancel_after", &[]))
        .await
        .unwrap()
        .iter()
        .map(|r| r.get("id"))
        .collect();
    assert_eq!(ids, [1]);
}

#[tokio::test]
#[ignore]
async fn a_request_given_up_on_while_queued_never_runs() {
    let mut client = connect().await;
    client
        .batch("CREATE TABLE #tdsql_cancel_queued (id INT)")
        .await
        .unwrap();

    // Polled once, the request is queued, then given up on. The test runtime
    // has one thread, so the connection can't get to it in between.
    let insert = client.execute("INSERT INTO #tdsql_cancel_queued (id) VALUES (1)", &[]);
    assert!(insert.now_or_never().is_none());

    let count: i32 =
        promptly(client.query_scalar("SELECT COUNT(*) FROM #tdsql_cancel_queued", &[]))
            .await
            .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
#[ignore]
async fn cancelling_inside_a_transaction_leaves_it_open() {
    let mut client = connect().await;
    client
        .batch("CREATE TABLE #tdsql_cancel_tx (id INT)")
        .await
        .unwrap();

    let mut tx = client.transaction().await.unwrap();
    tx.execute("INSERT INTO #tdsql_cancel_tx (id) VALUES (1)", &[])
        .await
        .unwrap();
    give_up(tx.query(ENDLESS, &[])).await;

    // The cancel stopped the query, not the transaction around it.
    let depth: i32 = promptly(tx.query_scalar("SELECT @@TRANCOUNT", &[]))
        .await
        .unwrap();
    assert_eq!(depth, 1);
    tx.commit().await.unwrap();

    let count: i32 = client
        .query_scalar("SELECT COUNT(*) FROM #tdsql_cancel_tx", &[])
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
#[ignore]
async fn cancelling_after_the_server_has_finished_leaves_the_connection_clean() {
    let mut client = connect().await;

    // More rows than the connection reads ahead of a stream, so it is still
    // partway through the response when the stream is dropped -- but few
    // enough that by then the server has sent them all and finished. The
    // cancel reaches a server with nothing left to stop, which still has to
    // acknowledge it.
    let mut rows = client
        .query_stream(
            "SELECT TOP (1000) CAST(1 AS INT) AS n FROM sys.all_columns",
            &[],
        )
        .await
        .unwrap();
    rows.try_next().await.unwrap().unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    drop(rows);

    assert_free(&mut client).await;
}

/// Sends nothing back for half a minute, like a statement still sorting,
/// aggregating or waiting on a lock.
const SILENT: &str = "WAITFOR DELAY '00:00:30'; SELECT 1 AS n";

#[tokio::test]
#[ignore]
async fn giving_up_on_a_query_that_has_sent_nothing_back_cancels_it() {
    let mut client = connect().await;
    give_up(client.query(SILENT, &[])).await;
    assert_free(&mut client).await;
}

#[tokio::test]
#[ignore]
async fn giving_up_on_execute_cancels_it() {
    let mut client = connect().await;
    client
        .batch("CREATE TABLE #tdsql_cancel_execute (id INT)")
        .await
        .unwrap();

    give_up(client.execute(
        "WAITFOR DELAY '00:00:30'; INSERT INTO #tdsql_cancel_execute (id) VALUES (1)",
        &[],
    ))
    .await;

    // The INSERT after the WAITFOR never ran.
    let count: i32 =
        promptly(client.query_scalar("SELECT COUNT(*) FROM #tdsql_cancel_execute", &[]))
            .await
            .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
#[ignore]
async fn giving_up_and_dropping_the_client_stops_the_query_on_the_server() {
    // Connected first, so it can't be handed the client's session ID.
    let mut observer = connect().await;
    let mut client = connect().await;
    let session: i16 = client.query_scalar("SELECT @@SPID", &[]).await.unwrap();
    give_up(client.query(SILENT, &[])).await;
    drop(client);

    promptly(async {
        while observer
            .query_scalar::<i32>(
                "SELECT COUNT(*) FROM sys.dm_exec_requests WHERE session_id = @P1",
                &[&session],
            )
            .await
            .unwrap()
            > 0
        {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
}
