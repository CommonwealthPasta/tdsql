//! Concurrency integration tests: several clients and transactions at once.
//!
//! A service that checks connections out of a pool leans on guarantees the
//! single-client tests never exercise — that one connection cannot see
//! another's uncommitted work, that a contended row really does block, that a
//! deadlock victim is reported as one, and that a connection stays usable
//! afterwards.
//!
//! ```text
//! docker run -e ACCEPT_EULA=Y -e SA_PASSWORD='YourStrong!Passw0rd' \
//!     -p 1433:1433 -d mcr.microsoft.com/mssql/server:2022-latest
//! cargo test --test concurrency_test -- --ignored
//! ```

use std::time::Duration;
use tdsql::{Client, Config, Error, IsolationLevel};

/// SQL Server error 1222, "Lock request time out period exceeded". Used to
/// turn "this blocks forever" into a fast, assertable failure.
const LOCK_TIMEOUT: u32 = 1222;

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

/// Each test gets its own table, so they can run concurrently.
async fn fresh_table(client: &mut Client, name: &str) {
    client
        .batch(&format!(
            "IF OBJECT_ID('dbo.{name}', 'U') IS NOT NULL DROP TABLE dbo.{name};
             CREATE TABLE dbo.{name} (id INT PRIMARY KEY, val INT NOT NULL)"
        ))
        .await
        .unwrap();
}

async fn seed(client: &mut Client, name: &str, rows: &[i32]) {
    for id in rows {
        client
            .execute(
                &format!("INSERT INTO dbo.{name} (id, val) VALUES (@P1, @P2)"),
                &[id, id],
            )
            .await
            .unwrap();
    }
}

async fn ids(client: &mut Client, name: &str) -> Vec<i32> {
    client
        .query(&format!("SELECT id FROM dbo.{name} ORDER BY id"), &[])
        .await
        .unwrap()
        .iter()
        .map(|r| r.get::<i32, _>("id"))
        .collect()
}

async fn count(client: &mut Client, name: &str) -> i32 {
    client
        .query_scalar(&format!("SELECT COUNT(*) FROM dbo.{name}"), &[])
        .await
        .unwrap()
}

async fn drop_table(client: &mut Client, name: &str) {
    client
        .batch(&format!("DROP TABLE IF EXISTS dbo.{name}"))
        .await
        .unwrap();
}

/// A second connection must not observe work the first has not committed. It
/// is blocked by the writer's lock rather than reading the row, so the lock
/// timeout is what proves the guarantee held.
#[tokio::test]
#[ignore]
async fn uncommitted_insert_is_not_visible_to_another_client() {
    let mut a = connect().await;
    let mut b = connect().await;
    fresh_table(&mut a, "cc_dirty").await;

    // Without this the assertion below would hang instead of failing.
    b.batch("SET LOCK_TIMEOUT 2000").await.unwrap();

    let mut tx = a.transaction().await.unwrap();
    tx.execute(
        "INSERT INTO dbo.cc_dirty (id, val) VALUES (@P1, @P2)",
        &[&1i32, &10i32],
    )
    .await
    .unwrap();

    let err = b
        .query_scalar::<i32>("SELECT COUNT(*) FROM dbo.cc_dirty", &[])
        .await
        .expect_err("a read-committed reader must not return the uncommitted row");
    assert_eq!(
        err.code(),
        Some(LOCK_TIMEOUT),
        "expected to be blocked by A's lock, got: {err}"
    );

    tx.rollback().await.unwrap();

    assert_eq!(
        count(&mut b, "cc_dirty").await,
        0,
        "a rolled-back insert must never be visible"
    );

    drop_table(&mut a, "cc_dirty").await;
}

/// The other half of the guarantee: once A commits, B sees the work on its own
/// pre-existing connection, without reconnecting.
#[tokio::test]
#[ignore]
async fn committed_insert_becomes_visible_to_another_client() {
    let mut a = connect().await;
    let mut b = connect().await;
    fresh_table(&mut a, "cc_commit").await;

    assert_eq!(count(&mut b, "cc_commit").await, 0);

    let mut tx = a.transaction().await.unwrap();
    tx.execute(
        "INSERT INTO dbo.cc_commit (id, val) VALUES (@P1, @P2)",
        &[&1i32, &10i32],
    )
    .await
    .unwrap();
    tx.execute(
        "INSERT INTO dbo.cc_commit (id, val) VALUES (@P1, @P2)",
        &[&2i32, &20i32],
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(ids(&mut b, "cc_commit").await, vec![1, 2]);

    drop_table(&mut a, "cc_commit").await;
}

/// A rollback on one connection must leave another connection's view alone,
/// including work it had already committed.
#[tokio::test]
#[ignore]
async fn rollback_by_one_client_leaves_another_untouched() {
    let mut a = connect().await;
    let mut b = connect().await;
    fresh_table(&mut a, "cc_rollback").await;
    seed(&mut b, "cc_rollback", &[1]).await;

    let mut tx = a.transaction().await.unwrap();
    tx.execute(
        "INSERT INTO dbo.cc_rollback (id, val) VALUES (@P1, @P2)",
        &[&2i32, &20i32],
    )
    .await
    .unwrap();
    tx.rollback().await.unwrap();

    assert_eq!(
        ids(&mut b, "cc_rollback").await,
        vec![1],
        "B's committed row survives, A's rolled-back row is gone"
    );

    drop_table(&mut a, "cc_rollback").await;
}

/// `IsolationLevel::ReadUncommitted` has to actually change what a second
/// connection sees, not merely be accepted by the server.
#[tokio::test]
#[ignore]
async fn read_uncommitted_sees_another_clients_dirty_row() {
    let mut a = connect().await;
    let mut b = connect().await;
    fresh_table(&mut a, "cc_dirty_read").await;

    let mut tx_a = a.transaction().await.unwrap();
    tx_a.execute(
        "INSERT INTO dbo.cc_dirty_read (id, val) VALUES (@P1, @P2)",
        &[&1i32, &10i32],
    )
    .await
    .unwrap();

    // A dirty reader takes no shared locks, so this returns rather than blocks.
    let mut tx_b = b
        .transaction_with_isolation(IsolationLevel::ReadUncommitted)
        .await
        .unwrap();
    let n: i32 = tx_b
        .query_scalar("SELECT COUNT(*) FROM dbo.cc_dirty_read", &[])
        .await
        .unwrap();
    assert_eq!(n, 1, "read uncommitted should see A's in-flight insert");
    tx_b.rollback().await.unwrap();

    tx_a.rollback().await.unwrap();

    assert_eq!(
        count(&mut b, "cc_dirty_read").await,
        0,
        "and the dirty row disappears once A rolls back"
    );

    drop_table(&mut a, "cc_dirty_read").await;
}

/// Two connections writing the same row must serialise: the second waits for
/// the first to commit, then succeeds on retry.
#[tokio::test]
#[ignore]
async fn write_write_contention_blocks_until_commit() {
    let mut a = connect().await;
    let mut b = connect().await;
    fresh_table(&mut a, "cc_contend").await;
    seed(&mut a, "cc_contend", &[1]).await;

    b.batch("SET LOCK_TIMEOUT 2000").await.unwrap();

    let mut tx = a.transaction().await.unwrap();
    tx.execute("UPDATE dbo.cc_contend SET val = 100 WHERE id = 1", &[])
        .await
        .unwrap();

    let err = b
        .execute("UPDATE dbo.cc_contend SET val = 200 WHERE id = 1", &[])
        .await
        .expect_err("B must not overwrite a row A has locked");
    assert_eq!(
        err.code(),
        Some(LOCK_TIMEOUT),
        "expected to wait on A's write lock, got: {err}"
    );

    tx.commit().await.unwrap();

    // With the lock released the same statement goes through.
    let affected = b
        .execute("UPDATE dbo.cc_contend SET val = 200 WHERE id = 1", &[])
        .await
        .expect("the update should succeed once A has committed");
    assert_eq!(affected, 1);

    let val: i32 = a
        .query_scalar("SELECT val FROM dbo.cc_contend WHERE id = 1", &[])
        .await
        .unwrap();
    assert_eq!(val, 200, "B's write is the surviving one");

    drop_table(&mut a, "cc_contend").await;
}

/// Two transactions grabbing each other's rows deadlock. The server picks a
/// victim and reports 1205; `is_deadlock()` is the retry signal, and both
/// connections have to remain usable afterwards.
#[tokio::test]
#[ignore]
async fn deadlock_victim_is_reported_and_connections_survive() {
    let mut a = connect().await;
    let mut b = connect().await;
    fresh_table(&mut a, "cc_deadlock").await;
    seed(&mut a, "cc_deadlock", &[1, 2]).await;

    let mut tx_a = a.transaction().await.unwrap();
    let mut tx_b = b.transaction().await.unwrap();

    // Each takes one row...
    tx_a.execute("UPDATE dbo.cc_deadlock SET val = 10 WHERE id = 1", &[])
        .await
        .unwrap();
    tx_b.execute("UPDATE dbo.cc_deadlock SET val = 20 WHERE id = 2", &[])
        .await
        .unwrap();

    // ...then reaches for the other's, which can only end one way. The
    // deadlock monitor runs on a timer, so this is not instant; the outer
    // timeout turns a missing detection into a failure rather than a hang.
    let (ra, rb) = tokio::time::timeout(Duration::from_secs(30), async {
        tokio::join!(
            tx_a.execute("UPDATE dbo.cc_deadlock SET val = 11 WHERE id = 2", &[]),
            tx_b.execute("UPDATE dbo.cc_deadlock SET val = 21 WHERE id = 1", &[]),
        )
    })
    .await
    .expect("the server should have picked a deadlock victim");

    let victims: Vec<&Error> = [ra.as_ref(), rb.as_ref()]
        .into_iter()
        .filter_map(|r| r.err())
        .collect();
    assert_eq!(
        victims.len(),
        1,
        "exactly one of the two statements should have been killed"
    );
    assert!(
        victims[0].is_deadlock(),
        "the victim should report error 1205, got: {}",
        victims[0]
    );

    // The victim's transaction is already gone server-side; dropping both
    // handles is how a caller would unwind.
    drop(tx_a);
    drop(tx_b);

    // The point of the retry signal is that the connection is still good.
    assert_eq!(a.query_scalar::<i32>("SELECT 1", &[]).await.unwrap(), 1);
    assert_eq!(b.query_scalar::<i32>("SELECT 1", &[]).await.unwrap(), 1);

    drop_table(&mut a, "cc_deadlock").await;
}

/// The shape a pooled service actually runs: many independent connections,
/// each with its own transaction, committing into one table at the same time.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn many_clients_commit_in_parallel() {
    const CLIENTS: i32 = 8;
    const ROWS_EACH: i32 = 5;

    let mut setup = connect().await;
    fresh_table(&mut setup, "cc_parallel").await;

    let tasks: Vec<_> = (0..CLIENTS)
        .map(|worker| {
            tokio::spawn(async move {
                let mut client = connect().await;
                let mut tx = client.transaction().await.unwrap();
                for row in 0..ROWS_EACH {
                    tx.execute(
                        "INSERT INTO dbo.cc_parallel (id, val) VALUES (@P1, @P2)",
                        &[&(worker * ROWS_EACH + row), &worker],
                    )
                    .await
                    .unwrap();
                }
                tx.commit().await.unwrap();
                client.close().await.unwrap();
            })
        })
        .collect();

    for task in tasks {
        task.await.expect("worker panicked");
    }

    assert_eq!(
        ids(&mut setup, "cc_parallel").await,
        (0..CLIENTS * ROWS_EACH).collect::<Vec<_>>(),
        "every worker's committed rows should be present exactly once"
    );

    drop_table(&mut setup, "cc_parallel").await;
}

/// Concurrent transactions on separate connections must not see each other's
/// in-flight work, even while all of them are open at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn parallel_transactions_are_isolated_from_each_other() {
    const CLIENTS: i32 = 4;

    let mut setup = connect().await;
    fresh_table(&mut setup, "cc_isolated").await;
    // The lookup below must be an index seek. Without this it scans, taking
    // shared locks on siblings' uncommitted rows, and the workers deadlock
    // each other instead of testing anything.
    setup
        .batch("CREATE INDEX ix_cc_isolated_val ON dbo.cc_isolated (val)")
        .await
        .unwrap();

    let tasks: Vec<_> = (0..CLIENTS)
        .map(|worker| {
            tokio::spawn(async move {
                let mut client = connect().await;
                let mut tx = client.transaction().await.unwrap();
                tx.execute(
                    "INSERT INTO dbo.cc_isolated (id, val) VALUES (@P1, @P2)",
                    &[&worker, &worker],
                )
                .await
                .unwrap();

                // Only this worker's row is visible to this transaction, so a
                // leak from a sibling shows up as a count above one.
                let seen: i32 = tx
                    .query_scalar(
                        "SELECT COUNT(*) FROM dbo.cc_isolated WHERE val = @P1",
                        &[&worker],
                    )
                    .await
                    .unwrap();
                assert_eq!(seen, 1, "worker {worker} should see only its own row");

                tx.rollback().await.unwrap();
                client.close().await.unwrap();
            })
        })
        .collect();

    for task in tasks {
        task.await.expect("worker panicked");
    }

    assert_eq!(
        count(&mut setup, "cc_isolated").await,
        0,
        "every worker rolled back, so the table should be empty"
    );

    drop_table(&mut setup, "cc_isolated").await;
}
