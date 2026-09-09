//! Integration tests for the `Executor` abstraction.
//!
//! The point of the trait is that one helper works unchanged against a client
//! and against a transaction, so these tests call the *same* helper both ways
//! and check the transactional behaviour differs correctly.
//!
//! ```text
//! cargo test --test executor_test -- --ignored
//! ```

use tdsql::{Client, Config, Executor, Result};

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

async fn fresh_table(client: &mut Client, name: &str) {
    client
        .batch(&format!(
            "IF OBJECT_ID('dbo.{name}', 'U') IS NOT NULL DROP TABLE dbo.{name};
             CREATE TABLE dbo.{name} (id INT PRIMARY KEY)"
        ))
        .await
        .unwrap();
}

async fn drop_table(client: &mut Client, name: &str) {
    client
        .batch(&format!("DROP TABLE IF EXISTS dbo.{name}"))
        .await
        .unwrap();
}

// --- the helpers under test -------------------------------------------------
// Written once, against `&mut impl Executor`, with no idea where they run.

async fn insert(db: &mut impl Executor, table: &str, id: i32) -> Result<u64> {
    db.execute(
        &format!("INSERT INTO dbo.{table} (id) VALUES (@P1)"),
        &[&id],
    )
    .await
}

async fn count(db: &mut impl Executor, table: &str) -> Result<i32> {
    db.query_scalar(&format!("SELECT COUNT(*) FROM dbo.{table}"), &[])
        .await
}

async fn ids(db: &mut impl Executor, table: &str) -> Result<Vec<i32>> {
    Ok(db
        .query(&format!("SELECT id FROM dbo.{table} ORDER BY id"), &[])
        .await?
        .iter()
        .map(|r| r.get::<i32, _>("id"))
        .collect())
}

/// Opens its own atomic scope and then abandons it. On a client that is a
/// transaction; inside a transaction it is a savepoint.
async fn insert_then_abandon(db: &mut impl Executor, table: &str, id: i32) -> Result<()> {
    let mut scope = db.transaction().await?;
    scope
        .execute(
            &format!("INSERT INTO dbo.{table} (id) VALUES (@P1)"),
            &[&id],
        )
        .await?;
    scope.rollback().await
}

// --- tests ------------------------------------------------------------------

/// The same helper, called with a `&mut Client` and then with a
/// `&mut Transaction`, with no change at the call site beyond what is passed.
#[tokio::test]
#[ignore]
async fn one_helper_serves_both_a_client_and_a_transaction() {
    let mut client = connect().await;
    fresh_table(&mut client, "ex_both").await;

    // Straight to the connection: autocommitted.
    assert_eq!(insert(&mut client, "ex_both", 1).await.unwrap(), 1);
    assert_eq!(count(&mut client, "ex_both").await.unwrap(), 1);

    // Same helper inside a transaction that commits.
    let mut tx = client.transaction().await.unwrap();
    insert(&mut tx, "ex_both", 2).await.unwrap();
    assert_eq!(count(&mut tx, "ex_both").await.unwrap(), 2);
    tx.commit().await.unwrap();

    // Same helper inside a transaction that rolls back.
    let mut tx = client.transaction().await.unwrap();
    insert(&mut tx, "ex_both", 3).await.unwrap();
    tx.rollback().await.unwrap();

    assert_eq!(
        ids(&mut client, "ex_both").await.unwrap(),
        vec![1, 2],
        "the committed rows survive, the rolled-back one does not"
    );

    drop_table(&mut client, "ex_both").await;
}

/// A helper that opens its own scope nests instead of trying to begin a second
/// transaction on a connection that already has one.
#[tokio::test]
#[ignore]
async fn a_helper_can_open_its_own_scope_either_way() {
    let mut client = connect().await;
    fresh_table(&mut client, "ex_scope").await;

    // On a client the helper's scope is a real transaction: rolling it back
    // leaves the table untouched.
    insert_then_abandon(&mut client, "ex_scope", 1)
        .await
        .unwrap();
    assert_eq!(count(&mut client, "ex_scope").await.unwrap(), 0);

    // Inside a transaction the same helper opens a savepoint, so abandoning it
    // discards only its own work — the surrounding transaction survives and
    // still commits.
    let mut tx = client.transaction().await.unwrap();
    insert(&mut tx, "ex_scope", 10).await.unwrap();
    insert_then_abandon(&mut tx, "ex_scope", 11).await.unwrap();
    insert(&mut tx, "ex_scope", 12).await.unwrap();
    tx.commit().await.unwrap();

    assert_eq!(
        ids(&mut client, "ex_scope").await.unwrap(),
        vec![10, 12],
        "the inner scope rolled back without taking the outer one with it"
    );

    drop_table(&mut client, "ex_scope").await;
}

/// A generic helper nested inside another generic helper: the reborrow through
/// `&mut impl Executor` has to keep working an arbitrary number of levels deep.
#[tokio::test]
#[ignore]
async fn generic_helpers_compose() {
    async fn outer(db: &mut impl Executor, table: &str) -> Result<i32> {
        insert(db, table, 1).await?;
        inner(db, table).await
    }

    async fn inner(db: &mut impl Executor, table: &str) -> Result<i32> {
        insert(db, table, 2).await?;
        count(db, table).await
    }

    let mut client = connect().await;
    fresh_table(&mut client, "ex_compose").await;

    let mut tx = client.transaction().await.unwrap();
    assert_eq!(outer(&mut tx, "ex_compose").await.unwrap(), 2);
    tx.commit().await.unwrap();

    assert_eq!(ids(&mut client, "ex_compose").await.unwrap(), vec![1, 2]);

    drop_table(&mut client, "ex_compose").await;
}

/// The futures a generic helper returns must be `Send`, or none of this is
/// usable from a spawned task — which is where a pooled service runs it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn generic_helpers_are_spawnable() {
    let mut setup = connect().await;
    fresh_table(&mut setup, "ex_spawn").await;

    let task = tokio::spawn(async move {
        let mut client = connect().await;
        let mut tx = client.transaction().await.unwrap();
        insert(&mut tx, "ex_spawn", 1).await.unwrap();
        let seen = count(&mut tx, "ex_spawn").await.unwrap();
        tx.commit().await.unwrap();
        seen
    });

    assert_eq!(task.await.expect("spawned task panicked"), 1);
    assert_eq!(ids(&mut setup, "ex_spawn").await.unwrap(), vec![1]);

    drop_table(&mut setup, "ex_spawn").await;
}
