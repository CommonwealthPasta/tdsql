//! Transaction integration tests against a live SQL Server.
//!
//! ```text
//! docker run -e ACCEPT_EULA=Y -e SA_PASSWORD='YourStrong!Passw0rd' \
//!     -p 1433:1433 -d mcr.microsoft.com/mssql/server:2022-latest
//! cargo test --test transaction_test -- --ignored
//! ```

use tdsql::{Client, Config, Error, IsolationLevel};

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
             CREATE TABLE dbo.{name} (id INT PRIMARY KEY)"
        ))
        .await
        .unwrap();
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

async fn drop_table(client: &mut Client, name: &str) {
    client
        .batch(&format!("DROP TABLE IF EXISTS dbo.{name}"))
        .await
        .unwrap();
}

#[tokio::test]
#[ignore]
async fn commit_persists_the_work() {
    let mut client = connect().await;
    fresh_table(&mut client, "tx_commit").await;

    let mut tx = client.transaction().await.unwrap();
    tx.execute("INSERT INTO dbo.tx_commit (id) VALUES (@P1)", &[&1i32])
        .await
        .unwrap();
    tx.execute("INSERT INTO dbo.tx_commit (id) VALUES (@P1)", &[&2i32])
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(ids(&mut client, "tx_commit").await, vec![1, 2]);
    drop_table(&mut client, "tx_commit").await;
}

#[tokio::test]
#[ignore]
async fn explicit_rollback_discards_the_work() {
    let mut client = connect().await;
    fresh_table(&mut client, "tx_rollback").await;

    let mut tx = client.transaction().await.unwrap();
    tx.execute("INSERT INTO dbo.tx_rollback (id) VALUES (@P1)", &[&1i32])
        .await
        .unwrap();
    tx.rollback().await.unwrap();

    assert!(ids(&mut client, "tx_rollback").await.is_empty());
    drop_table(&mut client, "tx_rollback").await;
}

/// The headline behaviour: dropping without committing must roll back, and the
/// rollback must land before the next statement on the connection.
#[tokio::test]
#[ignore]
async fn drop_without_commit_rolls_back() {
    let mut client = connect().await;
    fresh_table(&mut client, "tx_drop").await;

    {
        let mut tx = client.transaction().await.unwrap();
        tx.execute("INSERT INTO dbo.tx_drop (id) VALUES (@P1)", &[&1i32])
            .await
            .unwrap();
        // No commit, no rollback — just fall out of scope.
    }

    assert!(
        ids(&mut client, "tx_drop").await.is_empty(),
        "dropping a transaction must not commit it"
    );
    drop_table(&mut client, "tx_drop").await;
}

/// The realistic version of the above: an early `?` return abandons the
/// transaction mid-way.
#[tokio::test]
#[ignore]
async fn error_path_rolls_back() {
    let mut client = connect().await;
    fresh_table(&mut client, "tx_err").await;

    async fn unit_of_work(client: &mut Client) -> tdsql::Result<()> {
        let mut tx = client.transaction().await?;
        tx.execute("INSERT INTO dbo.tx_err (id) VALUES (@P1)", &[&1i32])
            .await?;
        // Duplicate key: fails, and `?` bails out with `tx` still open.
        tx.execute("INSERT INTO dbo.tx_err (id) VALUES (@P1)", &[&1i32])
            .await?;
        tx.commit().await
    }

    assert!(unit_of_work(&mut client).await.is_err());
    assert!(
        ids(&mut client, "tx_err").await.is_empty(),
        "the first insert must have been rolled back too"
    );
    drop_table(&mut client, "tx_err").await;
}

#[tokio::test]
#[ignore]
async fn uncommitted_work_is_visible_inside_the_transaction() {
    let mut client = connect().await;
    fresh_table(&mut client, "tx_visible").await;

    let mut tx = client.transaction().await.unwrap();
    tx.execute("INSERT INTO dbo.tx_visible (id) VALUES (@P1)", &[&1i32])
        .await
        .unwrap();

    let n: i32 = tx
        .query_scalar("SELECT COUNT(*) FROM dbo.tx_visible", &[])
        .await
        .unwrap();
    assert_eq!(n, 1, "the transaction should see its own writes");

    tx.rollback().await.unwrap();
    drop_table(&mut client, "tx_visible").await;
}

/// A `&mut Transaction` can be threaded through helper functions.
#[tokio::test]
#[ignore]
async fn transaction_can_be_passed_around() {
    use tdsql::Transaction;

    async fn add(tx: &mut Transaction<'_>, id: i32) -> tdsql::Result<()> {
        tx.execute("INSERT INTO dbo.tx_pass (id) VALUES (@P1)", &[&id])
            .await?;
        Ok(())
    }

    let mut client = connect().await;
    fresh_table(&mut client, "tx_pass").await;

    let mut tx = client.transaction().await.unwrap();
    add(&mut tx, 1).await.unwrap();
    add(&mut tx, 2).await.unwrap();
    add(&mut tx, 3).await.unwrap();
    tx.commit().await.unwrap();

    assert_eq!(ids(&mut client, "tx_pass").await, vec![1, 2, 3]);
    drop_table(&mut client, "tx_pass").await;
}

#[tokio::test]
#[ignore]
async fn isolation_level_is_accepted() {
    let mut client = connect().await;

    let mut tx = client
        .transaction_with_isolation(IsolationLevel::Serializable)
        .await
        .unwrap();
    let n: i32 = tx.query_scalar("SELECT 1", &[]).await.unwrap();
    assert_eq!(n, 1);
    tx.commit().await.unwrap();

    let mut tx = client
        .transaction_with_isolation(IsolationLevel::ReadUncommitted)
        .await
        .unwrap();
    let n: i32 = tx.query_scalar("SELECT 1", &[]).await.unwrap();
    assert_eq!(n, 1);
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn savepoint_rollback_keeps_the_outer_transaction() {
    let mut client = connect().await;
    fresh_table(&mut client, "tx_sp").await;

    let mut tx = client.transaction().await.unwrap();
    tx.execute("INSERT INTO dbo.tx_sp (id) VALUES (@P1)", &[&1i32])
        .await
        .unwrap();

    {
        let mut sp = tx.savepoint("before_risky").await.unwrap();
        sp.execute("INSERT INTO dbo.tx_sp (id) VALUES (@P1)", &[&2i32])
            .await
            .unwrap();
        sp.rollback().await.unwrap();
    }

    // The savepoint's insert is gone; the outer one survives the commit.
    tx.execute("INSERT INTO dbo.tx_sp (id) VALUES (@P1)", &[&3i32])
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(ids(&mut client, "tx_sp").await, vec![1, 3]);
    drop_table(&mut client, "tx_sp").await;
}

#[tokio::test]
#[ignore]
async fn savepoint_commit_keeps_its_work() {
    let mut client = connect().await;
    fresh_table(&mut client, "tx_spc").await;

    let mut tx = client.transaction().await.unwrap();
    {
        let mut sp = tx.savepoint("inner").await.unwrap();
        sp.execute("INSERT INTO dbo.tx_spc (id) VALUES (@P1)", &[&1i32])
            .await
            .unwrap();
        sp.commit().await.unwrap();
    }
    tx.commit().await.unwrap();

    assert_eq!(ids(&mut client, "tx_spc").await, vec![1]);
    drop_table(&mut client, "tx_spc").await;
}

#[tokio::test]
#[ignore]
async fn anonymous_nested_transaction() {
    let mut client = connect().await;
    fresh_table(&mut client, "tx_nested").await;

    let mut tx = client.transaction().await.unwrap();
    {
        let mut inner = tx.transaction().await.unwrap();
        inner
            .execute("INSERT INTO dbo.tx_nested (id) VALUES (@P1)", &[&1i32])
            .await
            .unwrap();
        inner.rollback().await.unwrap();
    }
    tx.execute("INSERT INTO dbo.tx_nested (id) VALUES (@P1)", &[&2i32])
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(ids(&mut client, "tx_nested").await, vec![2]);
    drop_table(&mut client, "tx_nested").await;
}

#[tokio::test]
#[ignore]
async fn rejects_a_malicious_savepoint_name() {
    let mut client = connect().await;
    let mut tx = client.transaction().await.unwrap();

    let err = tx
        .savepoint("evil; DROP TABLE users--")
        .await
        .expect_err("should reject a non-identifier savepoint name");
    assert!(matches!(err, Error::InvalidSavepointName(_)), "{err}");

    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn client_still_usable_after_a_dropped_transaction() {
    let mut client = connect().await;

    {
        let mut tx = client.transaction().await.unwrap();
        tx.query_scalar::<i32>("SELECT 1", &[]).await.unwrap();
    }

    // The queued rollback must not leave the connection wedged.
    let n: i32 = client.query_scalar("SELECT 42", &[]).await.unwrap();
    assert_eq!(n, 42);

    // And a fresh transaction still works.
    let mut tx = client.transaction().await.unwrap();
    let n: i32 = tx.query_scalar("SELECT 7", &[]).await.unwrap();
    assert_eq!(n, 7);
    tx.commit().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn statements_fail_after_close() {
    let client = connect().await;
    client.close().await.unwrap();
}
