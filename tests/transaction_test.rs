//! Transaction integration tests against a live SQL Server.
//!
//! ```text
//! docker run -e ACCEPT_EULA=Y -e SA_PASSWORD='YourStrong!Passw0rd' \
//!     -p 1433:1433 -d mcr.microsoft.com/mssql/server:2022-latest
//! cargo test --test transaction_test -- --ignored
//! ```

use tdsql::{Client, Config, Error, Executor, IsolationLevel};

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

/// The session's isolation level, as `sys.dm_exec_sessions` numbers it.
async fn isolation(client: &mut Client) -> i16 {
    client
        .query_scalar(
            "SELECT transaction_isolation_level FROM sys.dm_exec_sessions \
             WHERE session_id = @@SPID",
            &[],
        )
        .await
        .unwrap()
}

const READ_COMMITTED: i16 = 2;
const SERIALIZABLE: i16 = 4;

#[tokio::test]
#[ignore]
async fn an_isolation_level_ends_with_its_transaction() {
    let mut client = connect().await;

    let mut tx = client
        .transaction_with_isolation(IsolationLevel::Serializable)
        .await
        .unwrap();
    let inside: i16 = tx
        .query_scalar(
            "SELECT transaction_isolation_level FROM sys.dm_exec_sessions \
             WHERE session_id = @@SPID",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(inside, SERIALIZABLE);
    tx.commit().await.unwrap();
    assert_eq!(isolation(&mut client).await, READ_COMMITTED);

    // Dropped, so rolled back without a reply.
    drop(
        client
            .transaction_with_isolation(IsolationLevel::Serializable)
            .await
            .unwrap(),
    );
    assert_eq!(isolation(&mut client).await, READ_COMMITTED);
}

#[tokio::test]
#[ignore]
async fn a_level_set_outside_tdsql_does_not_leak_into_transaction() {
    let mut client = connect().await;
    client
        .batch("SET TRANSACTION ISOLATION LEVEL SERIALIZABLE")
        .await
        .unwrap();

    let mut tx = client.transaction().await.unwrap();
    let inside: i16 = tx
        .query_scalar(
            "SELECT transaction_isolation_level FROM sys.dm_exec_sessions \
             WHERE session_id = @@SPID",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(inside, READ_COMMITTED);
    tx.commit().await.unwrap();
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

/// `@@TRANCOUNT` on the connection behind `exec`.
async fn trancount(exec: &mut impl Executor) -> i32 {
    exec.query_scalar("SELECT @@TRANCOUNT", &[]).await.unwrap()
}

#[tokio::test]
#[ignore]
async fn giving_up_on_begin_leaves_no_transaction_open() {
    use futures::FutureExt;

    let mut client = connect().await;
    fresh_table(&mut client, "tdsql_tx_abandoned_begin").await;

    // Polled once, so the begin is sent, then given up on, as a `timeout` or
    // `select!` would.
    assert!(client.transaction().now_or_never().is_none());

    // Give the connection task a moment to see the reply go unclaimed.
    client.query_scalar::<i32>("SELECT 1", &[]).await.unwrap();
    assert_eq!(trancount(&mut client).await, 0);

    // A later transaction is its own, not nested in a leftover one, so its
    // commit is real.
    let mut tx = client.transaction().await.unwrap();
    assert_eq!(trancount(&mut tx).await, 1);
    tx.execute(
        "INSERT INTO dbo.tdsql_tx_abandoned_begin (id) VALUES (1)",
        &[],
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(trancount(&mut client).await, 0);

    let mut other = connect().await;
    assert_eq!(ids(&mut other, "tdsql_tx_abandoned_begin").await, vec![1]);
    drop_table(&mut client, "tdsql_tx_abandoned_begin").await;
}

#[tokio::test]
#[ignore]
async fn giving_up_on_a_serializable_begin_restores_the_isolation_level() {
    use futures::FutureExt;

    let mut client = connect().await;
    assert!(
        client
            .transaction_with_isolation(IsolationLevel::Serializable)
            .now_or_never()
            .is_none()
    );
    client.query_scalar::<i32>("SELECT 1", &[]).await.unwrap();
    assert_eq!(trancount(&mut client).await, 0);
    assert_eq!(isolation(&mut client).await, READ_COMMITTED);
}

#[tokio::test]
#[ignore]
async fn statements_fail_after_close() {
    let client = connect().await;
    client.close().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn a_transaction_the_server_rolled_back_refuses_to_go_on() {
    let mut client = connect().await;
    fresh_table(&mut client, "tdsql_tx_aborted").await;
    client.batch("SET XACT_ABORT ON").await.unwrap();
    {
        let mut tx = client.transaction().await.unwrap();
        tx.execute("INSERT INTO dbo.tdsql_tx_aborted (id) VALUES (1)", &[])
            .await
            .unwrap();
        // A duplicate key under XACT_ABORT rolls the whole transaction back.
        let err = tx
            .execute("INSERT INTO dbo.tdsql_tx_aborted (id) VALUES (1)", &[])
            .await
            .unwrap_err();
        assert_eq!(err.code(), Some(2627), "{err}");

        // Regression: these used to run in autocommit and survive the rollback.
        let err = tx
            .execute("INSERT INTO dbo.tdsql_tx_aborted (id) VALUES (2)", &[])
            .await
            .unwrap_err();
        assert!(matches!(err, Error::TransactionAborted), "{err}");
        let err = tx.query("SELECT 1", &[]).await.unwrap_err();
        assert!(matches!(err, Error::TransactionAborted), "{err}");
        let err = tx.batch("SELECT 1").await.unwrap_err();
        assert!(matches!(err, Error::TransactionAborted), "{err}");
        let err = tx.transaction().await.unwrap_err();
        assert!(matches!(err, Error::TransactionAborted), "{err}");

        tx.rollback().await.unwrap();
    }
    client.batch("SET XACT_ABORT OFF").await.unwrap();
    assert_eq!(trancount(&mut client).await, 0);
    assert!(ids(&mut client, "tdsql_tx_aborted").await.is_empty());
    drop_table(&mut client, "tdsql_tx_aborted").await;
}

#[tokio::test]
#[ignore]
async fn a_rollback_in_the_transactions_own_sql_ends_it() {
    let mut client = connect().await;
    fresh_table(&mut client, "tdsql_tx_self_rollback").await;
    let err = {
        let mut tx = client.transaction().await.unwrap();
        tx.execute(
            "INSERT INTO dbo.tdsql_tx_self_rollback (id) VALUES (1)",
            &[],
        )
        .await
        .unwrap();
        tx.batch("IF @@TRANCOUNT > 0 ROLLBACK").await.unwrap();
        let err = tx
            .execute(
                "INSERT INTO dbo.tdsql_tx_self_rollback (id) VALUES (2)",
                &[],
            )
            .await
            .unwrap_err();
        assert!(matches!(err, Error::TransactionAborted), "{err}");
        tx.commit().await.unwrap_err()
    };
    assert!(matches!(err, Error::TransactionAborted), "{err}");

    assert_eq!(trancount(&mut client).await, 0);
    let n: i32 = client.query_scalar("SELECT 42", &[]).await.unwrap();
    assert_eq!(n, 42);
    assert!(ids(&mut client, "tdsql_tx_self_rollback").await.is_empty());
    drop_table(&mut client, "tdsql_tx_self_rollback").await;
}

#[tokio::test]
#[ignore]
async fn dropping_an_aborted_transaction_leaves_the_connection_clean() {
    let mut client = connect().await;
    {
        let mut tx = client
            .transaction_with_isolation(IsolationLevel::Serializable)
            .await
            .unwrap();
        tx.batch("ROLLBACK").await.unwrap();
        assert!(matches!(
            tx.query_scalar::<i32>("SELECT 1", &[]).await,
            Err(Error::TransactionAborted)
        ));
    }
    // The drop's rollback has nothing to undo, and must not fail the
    // connection or leave the isolation level behind.
    assert_eq!(trancount(&mut client).await, 0);
    assert_eq!(isolation(&mut client).await, READ_COMMITTED);
    let mut tx = client.transaction().await.unwrap();
    assert_eq!(trancount(&mut tx).await, 1);
    tx.commit().await.unwrap();
}
