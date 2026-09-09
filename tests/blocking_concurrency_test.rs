//! Concurrency integration tests for the blocking client.
//!
//! Each blocking client owns its own runtime, so "concurrent" here means real
//! OS threads — the shape a synchronous service with a thread pool runs.
//!
//! ```text
//! cargo test --features blocking --test blocking_concurrency_test -- --ignored
//! ```

#![cfg(feature = "blocking")]

use std::thread;
use tdsql::blocking::Client;
use tdsql::Config;

/// SQL Server error 1222, "Lock request time out period exceeded".
const LOCK_TIMEOUT: u32 = 1222;

fn test_config() -> Config {
    Config::new()
        .host("localhost")
        .port(1433)
        .database("master")
        .auth("sa", "YourStrong!Passw0rd")
        .trust_cert()
}

fn connect() -> Client {
    Client::connect(&test_config())
        .expect("failed to connect; is SQL Server running on localhost:1433?")
}

fn fresh_table(client: &mut Client, name: &str) {
    client
        .batch(&format!(
            "IF OBJECT_ID('dbo.{name}', 'U') IS NOT NULL DROP TABLE dbo.{name};
             CREATE TABLE dbo.{name} (id INT PRIMARY KEY, val INT NOT NULL)"
        ))
        .unwrap();
}

fn ids(client: &mut Client, name: &str) -> Vec<i32> {
    client
        .query(&format!("SELECT id FROM dbo.{name} ORDER BY id"), &[])
        .unwrap()
        .iter()
        .map(|r| r.get::<i32, _>("id"))
        .collect()
}

fn count(client: &mut Client, name: &str) -> i32 {
    client
        .query_scalar(&format!("SELECT COUNT(*) FROM dbo.{name}"), &[])
        .unwrap()
}

fn drop_table(client: &mut Client, name: &str) {
    client
        .batch(&format!("DROP TABLE IF EXISTS dbo.{name}"))
        .unwrap();
}

/// Two blocking clients on one thread: the calls are synchronous, but A's
/// transaction stays open on the server across B's, so the isolation
/// guarantee is the same one the async client relies on.
#[test]
#[ignore]
fn uncommitted_insert_is_not_visible_to_another_blocking_client() {
    let mut a = connect();
    let mut b = connect();
    fresh_table(&mut a, "bcc_dirty");

    // Without this the call below would block the thread indefinitely.
    b.batch("SET LOCK_TIMEOUT 2000").unwrap();

    let mut tx = a.transaction().unwrap();
    tx.execute(
        "INSERT INTO dbo.bcc_dirty (id, val) VALUES (@P1, @P2)",
        &[&1i32, &10i32],
    )
    .unwrap();

    let err = b
        .query_scalar::<i32>("SELECT COUNT(*) FROM dbo.bcc_dirty", &[])
        .expect_err("a read-committed reader must not return the uncommitted row");
    assert_eq!(
        err.code(),
        Some(LOCK_TIMEOUT),
        "expected to be blocked by A's lock, got: {err}"
    );

    tx.rollback().unwrap();

    assert_eq!(
        count(&mut b, "bcc_dirty"),
        0,
        "a rolled-back insert must never be visible"
    );

    drop_table(&mut a, "bcc_dirty");
}

/// Several threads, one blocking client and one transaction each, committing
/// into the same table at once. Each client drives its own runtime, so this
/// also checks those runtimes do not tread on each other.
#[test]
#[ignore]
fn blocking_clients_commit_in_parallel() {
    const THREADS: i32 = 4;
    const ROWS_EACH: i32 = 5;

    let mut setup = connect();
    fresh_table(&mut setup, "bcc_parallel");

    let workers: Vec<_> = (0..THREADS)
        .map(|worker| {
            thread::spawn(move || {
                let mut client = connect();
                let mut tx = client.transaction().unwrap();
                for row in 0..ROWS_EACH {
                    tx.execute(
                        "INSERT INTO dbo.bcc_parallel (id, val) VALUES (@P1, @P2)",
                        &[&(worker * ROWS_EACH + row), &worker],
                    )
                    .unwrap();
                }
                tx.commit().unwrap();
                client.close().unwrap();
            })
        })
        .collect();

    for worker in workers {
        worker.join().expect("worker thread panicked");
    }

    assert_eq!(
        ids(&mut setup, "bcc_parallel"),
        (0..THREADS * ROWS_EACH).collect::<Vec<_>>(),
        "every thread's committed rows should be present exactly once"
    );

    drop_table(&mut setup, "bcc_parallel");
}

/// A rollback on one thread must not discard another thread's committed work.
#[test]
#[ignore]
fn parallel_blocking_rollbacks_do_not_touch_committed_work() {
    const THREADS: i32 = 4;

    let mut setup = connect();
    fresh_table(&mut setup, "bcc_rollback");

    let workers: Vec<_> = (0..THREADS)
        .map(|worker| {
            thread::spawn(move || {
                let mut client = connect();

                // Odd workers commit, even workers roll back.
                let mut tx = client.transaction().unwrap();
                tx.execute(
                    "INSERT INTO dbo.bcc_rollback (id, val) VALUES (@P1, @P2)",
                    &[&worker, &worker],
                )
                .unwrap();
                if worker % 2 == 0 {
                    tx.rollback().unwrap();
                } else {
                    tx.commit().unwrap();
                }
                client.close().unwrap();
            })
        })
        .collect();

    for worker in workers {
        worker.join().expect("worker thread panicked");
    }

    let expected: Vec<i32> = (0..THREADS).filter(|w| w % 2 != 0).collect();
    assert_eq!(
        ids(&mut setup, "bcc_rollback"),
        expected,
        "only the committing threads' rows should survive"
    );

    drop_table(&mut setup, "bcc_rollback");
}
