//! Streaming integration tests against a live SQL Server.
//!
//! ```text
//! cargo test --test stream_test -- --ignored
//! ```

use std::time::{Duration, Instant};

use tdsql::{Client, Config, Error, Executor};

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

/// `@P1` rows numbered from 1, in order.
///
/// `TOP` goes inside so only those rows are sorted: with it outside, the server
/// sorts the whole cross join first, which takes it tens of seconds.
const NUMBERS: &str = "SELECT n FROM (SELECT TOP (@P1) \
     CAST(ROW_NUMBER() OVER (ORDER BY (SELECT NULL)) AS INT) AS n \
     FROM sys.all_columns a CROSS JOIN sys.all_columns b) AS numbers \
     ORDER BY n";

#[tokio::test]
#[ignore]
async fn streams_every_row_in_order() {
    let mut client = connect().await;
    let mut rows = client.query_stream(NUMBERS, &[&5000i32]).await.unwrap();

    let mut expected = 1;
    while let Some(row) = rows.try_next().await.unwrap() {
        assert_eq!(row.get::<i32, _>("n"), expected);
        expected += 1;
    }
    assert_eq!(expected, 5001);
}

#[tokio::test]
#[ignore]
async fn rows_arrive_before_the_query_finishes() {
    let mut client = connect().await;
    let started = Instant::now();

    // The row is flushed to the client, then the request stays open for two
    // more seconds. Collecting would see nothing until the end.
    let mut rows = client
        .query_stream(
            "SELECT 1 AS n; \
             RAISERROR('flush', 0, 1) WITH NOWAIT; \
             WAITFOR DELAY '00:00:02';",
            &[],
        )
        .await
        .unwrap();

    let row = rows.try_next().await.unwrap().unwrap();
    let first_row = started.elapsed();
    assert_eq!(row.get::<i32, _>("n"), 1);

    assert!(rows.try_next().await.unwrap().is_none());
    let finished = started.elapsed();

    assert!(
        first_row < Duration::from_secs(1),
        "first row took {first_row:?}"
    );
    assert!(
        finished >= Duration::from_secs(2),
        "stream ended after {finished:?}, before the request did"
    );
}

#[tokio::test]
#[ignore]
async fn columns_are_known_before_any_row() {
    let mut client = connect().await;
    let mut rows = client
        .query_stream("SELECT CAST(1 AS INT) AS a, N'x' AS b WHERE 1 = 0", &[])
        .await
        .unwrap();

    let names: Vec<&str> = rows.columns().iter().map(|c| c.name()).collect();
    assert_eq!(names, ["a", "b"]);
    assert!(rows.try_next().await.unwrap().is_none());
}

#[tokio::test]
#[ignore]
async fn a_statement_without_a_result_set_streams_nothing() {
    let mut client = connect().await;
    let mut rows = client
        .query_stream("DECLARE @x INT = 1", &[])
        .await
        .unwrap();

    assert!(rows.columns().is_empty());
    assert!(rows.try_next().await.unwrap().is_none());
}

#[tokio::test]
#[ignore]
async fn a_rejected_statement_fails_up_front() {
    let mut client = connect().await;
    let err = client
        .query_stream("SELECT id FROM dbo.tdsql_no_such_table", &[])
        .await
        .unwrap_err();

    assert_eq!(err.code(), Some(208));
    assert_eq!(
        err.statement(),
        Some("SELECT id FROM dbo.tdsql_no_such_table")
    );

    // And the connection is fine afterwards.
    let n: i32 = client.query_scalar("SELECT 42", &[]).await.unwrap();
    assert_eq!(n, 42);
}

#[tokio::test]
#[ignore]
async fn only_the_first_result_set_is_streamed() {
    let mut client = connect().await;
    let mut rows = client
        .query_stream("SELECT 1 AS a; SELECT 2 AS b", &[])
        .await
        .unwrap();

    let row = rows.try_next().await.unwrap().unwrap();
    assert_eq!(row.get::<i32, _>("a"), 1);
    assert!(rows.try_next().await.unwrap().is_none());
}

#[tokio::test]
#[ignore]
async fn a_later_failure_follows_the_rows_before_it() {
    let mut client = connect().await;
    let mut rows = client
        .query_stream("SELECT 1 AS a; SELECT 1 / 0 AS boom", &[])
        .await
        .unwrap();

    let row = rows.try_next().await.unwrap().unwrap();
    assert_eq!(row.get::<i32, _>("a"), 1);

    let err = rows.try_next().await.unwrap_err();
    assert_eq!(err.code(), Some(8134), "divide by zero, got {err}");
    assert!(err.statement().is_some_and(|s| s.contains("1 / 0")));

    // The failure ends the stream.
    assert!(rows.try_next().await.unwrap().is_none());
}

#[tokio::test]
#[ignore]
async fn dropping_early_leaves_the_connection_clean() {
    let mut client = connect().await;

    let mut rows = client.query_stream(NUMBERS, &[&50_000i32]).await.unwrap();
    for _ in 0..10 {
        rows.try_next().await.unwrap().unwrap();
    }
    drop(rows);

    // Nothing left over from the abandoned stream leaks into what follows.
    let n: i32 = client.query_scalar("SELECT 42", &[]).await.unwrap();
    assert_eq!(n, 42);

    let all = client.query(NUMBERS, &[&3i32]).await.unwrap();
    let ns: Vec<i32> = all.iter().map(|r| r.get("n")).collect();
    assert_eq!(ns, [1, 2, 3]);
}

#[tokio::test]
#[ignore]
async fn a_slow_consumer_still_gets_every_row() {
    let mut client = connect().await;
    // Comfortably more than the read-ahead, so the connection task has to
    // wait on the consumer repeatedly.
    let mut rows = client.query_stream(NUMBERS, &[&500i32]).await.unwrap();

    let mut expected = 1;
    while let Some(row) = rows.try_next().await.unwrap() {
        assert_eq!(row.get::<i32, _>("n"), expected);
        expected += 1;
        if expected % 50 == 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    assert_eq!(expected, 501);
}

#[tokio::test]
#[ignore]
async fn abandoning_the_call_before_it_returns_leaves_the_connection_clean() {
    let mut client = connect().await;

    // Give up while the server is still in the WAITFOR, before any columns.
    let gave_up = tokio::time::timeout(
        Duration::from_millis(200),
        client.query_stream("WAITFOR DELAY '00:00:01'; SELECT 1 AS n", &[]),
    )
    .await;
    assert!(gave_up.is_err(), "the query should still have been running");

    // The next statement queues behind the abandoned one and sees its own
    // result, not the leftover `1`.
    let n: i32 = client.query_scalar("SELECT 42", &[]).await.unwrap();
    assert_eq!(n, 42);
}

#[tokio::test]
#[ignore]
async fn streams_inside_a_transaction() {
    let mut client = connect().await;
    client
        .batch("CREATE TABLE #tdsql_stream_tx (id INT)")
        .await
        .unwrap();

    let mut tx = client.transaction().await.unwrap();
    tx.execute("INSERT INTO #tdsql_stream_tx (id) VALUES (1), (2)", &[])
        .await
        .unwrap();

    // The transaction's own uncommitted rows are visible to its stream.
    let mut rows = tx
        .query_stream("SELECT id FROM #tdsql_stream_tx ORDER BY id", &[])
        .await
        .unwrap();
    assert_eq!(
        rows.try_next().await.unwrap().unwrap().get::<i32, _>("id"),
        1
    );
    drop(rows);

    tx.commit().await.unwrap();
    let count: i32 = client
        .query_scalar("SELECT COUNT(*) FROM #tdsql_stream_tx", &[])
        .await
        .unwrap();
    assert_eq!(count, 2);
}

/// Counts through the `Executor` trait, so it runs against either.
async fn count_streamed(db: &mut impl Executor) -> usize {
    let mut rows = db.query_stream(NUMBERS, &[&100i32]).await.unwrap();
    let mut count = 0;
    while rows.try_next().await.unwrap().is_some() {
        count += 1;
    }
    count
}

#[tokio::test]
#[ignore]
async fn streams_through_the_executor_trait() {
    let mut client = connect().await;
    assert_eq!(count_streamed(&mut client).await, 100);

    let mut tx = client.transaction().await.unwrap();
    assert_eq!(count_streamed(&mut tx).await, 100);
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn is_a_futures_stream() {
    use futures::TryStreamExt;

    let mut client = connect().await;
    let ns: Vec<i32> = client
        .query_stream(NUMBERS, &[&5i32])
        .await
        .unwrap()
        .map_ok(|row| row.get::<i32, _>("n"))
        .try_collect()
        .await
        .unwrap();
    assert_eq!(ns, [1, 2, 3, 4, 5]);
}

#[tokio::test]
#[ignore]
async fn a_conversion_failure_ends_the_stream_and_leaves_the_connection_clean() {
    let mut client = connect().await;

    // 10^30 fits DECIMAL(38,0) on the server but not `rust_decimal`, so it
    // fails in this crate, partway through, rather than on the server.
    let mut rows = client
        .query_stream(
            "SELECT d FROM (VALUES \
                 (1, CAST(1 AS DECIMAL(38, 0))), \
                 (2, POWER(CAST(10 AS DECIMAL(38, 0)), 30)), \
                 (3, CAST(3 AS DECIMAL(38, 0)))) AS v (k, d) \
             ORDER BY k",
            &[],
        )
        .await
        .unwrap();

    assert!(rows.try_next().await.unwrap().is_some());
    let err = rows.try_next().await.unwrap_err();
    assert!(matches!(err, Error::Decimal { .. }), "got {err}");
    assert!(rows.try_next().await.unwrap().is_none());
    drop(rows);

    let n: i32 = client.query_scalar("SELECT 42", &[]).await.unwrap();
    assert_eq!(n, 42);
}
