//! Transactions: commit, rollback-on-drop, and savepoints.
//!
//! ```text
//! cargo run --example transaction
//! ```

use tdsql::{Client, Config, IsolationLevel, Transaction};

#[tokio::main]
async fn main() -> tdsql::Result<()> {
    let mut client = Client::connect(
        &Config::new()
            .host("localhost")
            .database("master")
            .auth("sa", "YourStrong!Passw0rd")
            .trust_cert(),
    )
    .await?;

    client
        .batch(
            "IF OBJECT_ID('dbo.tdsql_tx_demo', 'U') IS NOT NULL DROP TABLE dbo.tdsql_tx_demo;
             CREATE TABLE dbo.tdsql_tx_demo (id INT PRIMARY KEY)",
        )
        .await?;

    // 1. Committing keeps the work.
    let mut tx = client.transaction().await?;
    add(&mut tx, 1).await?;
    add(&mut tx, 2).await?;
    tx.commit().await?;
    println!("after commit:            {:?}", ids(&mut client).await?);

    // 2. Dropping without committing discards it. No rollback() call here --
    //    the transaction simply goes out of scope.
    {
        let mut tx = client.transaction().await?;
        add(&mut tx, 99).await?;
    }
    println!("after drop (no commit):  {:?}", ids(&mut client).await?);

    // 3. An early `?` return is safe for the same reason.
    let failed = attempt_bad_insert(&mut client).await;
    println!("error path returned:     {}", failed.is_err());
    println!("after error path:        {:?}", ids(&mut client).await?);

    // 4. A savepoint is a nested transaction: rolling it back leaves the
    //    surrounding transaction untouched.
    let mut tx = client
        .transaction_with_isolation(IsolationLevel::Serializable)
        .await?;
    add(&mut tx, 3).await?;
    {
        let mut sp = tx.savepoint("before_risky").await?;
        add(&mut sp, 4).await?;
        sp.rollback().await?;
    }
    tx.commit().await?;
    println!("after savepoint rollback:{:?}", ids(&mut client).await?);

    client.batch("DROP TABLE dbo.tdsql_tx_demo").await?;
    client.close().await
}

async fn add(tx: &mut Transaction<'_>, id: i32) -> tdsql::Result<()> {
    tx.execute("INSERT INTO dbo.tdsql_tx_demo (id) VALUES (@P1)", &[&id])
        .await?;
    Ok(())
}

async fn attempt_bad_insert(client: &mut Client) -> tdsql::Result<()> {
    let mut tx = client.transaction().await?;
    add(&mut tx, 50).await?;
    // Duplicate key: this fails, and `?` returns with `tx` still open.
    add(&mut tx, 50).await?;
    tx.commit().await
}

async fn ids(client: &mut Client) -> tdsql::Result<Vec<i32>> {
    Ok(client
        .query("SELECT id FROM dbo.tdsql_tx_demo ORDER BY id", &[])
        .await?
        .iter()
        .map(|r| r.get::<i32, _>("id"))
        .collect())
}
