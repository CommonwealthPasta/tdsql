//! Integration tests for the blocking `Executor` abstraction.
//!
//! ```text
//! cargo test --features blocking --test blocking_executor_test -- --ignored
//! ```

#![cfg(feature = "blocking")]

use std::thread;
use tdsql::blocking::{Client, Executor};
use tdsql::{Config, Result};

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
             CREATE TABLE dbo.{name} (id INT PRIMARY KEY)"
        ))
        .unwrap();
}

fn drop_table(client: &mut Client, name: &str) {
    client
        .batch(&format!("DROP TABLE IF EXISTS dbo.{name}"))
        .unwrap();
}

// --- the helpers under test -------------------------------------------------
// Written once, against `&mut impl Executor`, with no idea where they run.

fn insert(db: &mut impl Executor, table: &str, id: i32) -> Result<u64> {
    db.execute(
        &format!("INSERT INTO dbo.{table} (id) VALUES (@P1)"),
        &[&id],
    )
}

fn count(db: &mut impl Executor, table: &str) -> Result<i32> {
    db.query_scalar(&format!("SELECT COUNT(*) FROM dbo.{table}"), &[])
}

fn ids(db: &mut impl Executor, table: &str) -> Result<Vec<i32>> {
    Ok(db
        .query(&format!("SELECT id FROM dbo.{table} ORDER BY id"), &[])?
        .iter()
        .map(|r| r.get::<i32, _>("id"))
        .collect())
}

/// Opens its own atomic scope and then abandons it. On a client that is a
/// transaction; inside a transaction it is a savepoint.
fn insert_then_abandon(db: &mut impl Executor, table: &str, id: i32) -> Result<()> {
    let mut scope = db.transaction()?;
    scope.execute(
        &format!("INSERT INTO dbo.{table} (id) VALUES (@P1)"),
        &[&id],
    )?;
    scope.rollback()
}

// --- tests ------------------------------------------------------------------

/// The same helper, called with a `&mut Client` and then with a
/// `&mut Transaction`.
#[test]
#[ignore]
fn one_helper_serves_both_a_client_and_a_transaction() {
    let mut client = connect();
    fresh_table(&mut client, "bex_both");

    // Straight to the connection: autocommitted.
    assert_eq!(insert(&mut client, "bex_both", 1).unwrap(), 1);
    assert_eq!(count(&mut client, "bex_both").unwrap(), 1);

    // Same helper inside a transaction that commits.
    let mut tx = client.transaction().unwrap();
    insert(&mut tx, "bex_both", 2).unwrap();
    assert_eq!(count(&mut tx, "bex_both").unwrap(), 2);
    tx.commit().unwrap();

    // Same helper inside a transaction that rolls back.
    let mut tx = client.transaction().unwrap();
    insert(&mut tx, "bex_both", 3).unwrap();
    tx.rollback().unwrap();

    assert_eq!(
        ids(&mut client, "bex_both").unwrap(),
        vec![1, 2],
        "the committed rows survive, the rolled-back one does not"
    );

    drop_table(&mut client, "bex_both");
}

/// A helper that opens its own scope nests instead of trying to begin a second
/// transaction on a connection that already has one.
#[test]
#[ignore]
fn a_helper_can_open_its_own_scope_either_way() {
    let mut client = connect();
    fresh_table(&mut client, "bex_scope");

    // On a client the helper's scope is a real transaction.
    insert_then_abandon(&mut client, "bex_scope", 1).unwrap();
    assert_eq!(count(&mut client, "bex_scope").unwrap(), 0);

    // Inside a transaction the same helper opens a savepoint, so abandoning it
    // discards only its own work.
    let mut tx = client.transaction().unwrap();
    insert(&mut tx, "bex_scope", 10).unwrap();
    insert_then_abandon(&mut tx, "bex_scope", 11).unwrap();
    insert(&mut tx, "bex_scope", 12).unwrap();
    tx.commit().unwrap();

    assert_eq!(
        ids(&mut client, "bex_scope").unwrap(),
        vec![10, 12],
        "the inner scope rolled back without taking the outer one with it"
    );

    drop_table(&mut client, "bex_scope");
}

/// A generic helper nested inside another generic helper: the reborrow through
/// `&mut impl Executor` has to keep working an arbitrary number of levels deep.
#[test]
#[ignore]
fn generic_helpers_compose() {
    fn outer(db: &mut impl Executor, table: &str) -> Result<i32> {
        insert(db, table, 1)?;
        inner(db, table)
    }

    fn inner(db: &mut impl Executor, table: &str) -> Result<i32> {
        insert(db, table, 2)?;
        count(db, table)
    }

    let mut client = connect();
    fresh_table(&mut client, "bex_compose");

    let mut tx = client.transaction().unwrap();
    assert_eq!(outer(&mut tx, "bex_compose").unwrap(), 2);
    tx.commit().unwrap();

    assert_eq!(ids(&mut client, "bex_compose").unwrap(), vec![1, 2]);

    drop_table(&mut client, "bex_compose");
}

/// The generic helpers have to work from a worker thread, which is where a
/// synchronous service actually runs them.
#[test]
#[ignore]
fn generic_helpers_work_across_threads() {
    const THREADS: i32 = 4;

    let mut setup = connect();
    fresh_table(&mut setup, "bex_threads");

    let workers: Vec<_> = (0..THREADS)
        .map(|worker| {
            thread::spawn(move || {
                let mut client = connect();
                let mut tx = client.transaction().unwrap();
                insert(&mut tx, "bex_threads", worker).unwrap();
                tx.commit().unwrap();
                client.close().unwrap();
            })
        })
        .collect();

    for worker in workers {
        worker.join().expect("worker thread panicked");
    }

    assert_eq!(
        ids(&mut setup, "bex_threads").unwrap(),
        (0..THREADS).collect::<Vec<_>>()
    );

    drop_table(&mut setup, "bex_threads");
}
