//! The blocking client: no async, no runtime to set up.
//!
//! ```text
//! cargo run --features blocking --example blocking
//! ```

use tdsql::blocking::Client;
use tdsql::Config;

fn main() -> tdsql::Result<()> {
    // Note: an ordinary `fn main`, not `#[tokio::main]`.
    let mut client = Client::connect(
        &Config::new()
            .host("localhost")
            .port(1433)
            .database("master")
            .auth("sa", "YourStrong!Passw0rd")
            .trust_cert(),
    )?;

    let version: String = client.query_scalar("SELECT @@VERSION", &[])?;
    println!(
        "connected to: {}",
        version.lines().next().unwrap_or_default()
    );

    // Typed reads, exactly as in the async client.
    let row = client.query_one("SELECT @P1 AS n, @P2 AS label", &[&42i32, &"answer"])?;
    let n: i32 = row.get("n");
    let label: String = row.get("label");
    println!("{label} = {n}");

    // Transactions work the same way, including rollback on drop.
    client.batch(
        "IF OBJECT_ID('dbo.tdsql_blocking_demo', 'U') IS NOT NULL
             DROP TABLE dbo.tdsql_blocking_demo;
         CREATE TABLE dbo.tdsql_blocking_demo (id INT PRIMARY KEY)",
    )?;

    let mut tx = client.transaction()?;
    tx.execute(
        "INSERT INTO dbo.tdsql_blocking_demo (id) VALUES (@P1)",
        &[&1i32],
    )?;
    tx.commit()?;

    {
        let mut tx = client.transaction()?;
        tx.execute(
            "INSERT INTO dbo.tdsql_blocking_demo (id) VALUES (@P1)",
            &[&99i32],
        )?;
        // Dropped without committing: discarded.
    }

    let ids: Vec<i32> = client
        .query("SELECT id FROM dbo.tdsql_blocking_demo ORDER BY id", &[])?
        .iter()
        .map(|r| r.get::<i32, _>("id"))
        .collect();
    println!("rows after commit + dropped transaction: {ids:?}");

    client.batch("DROP TABLE dbo.tdsql_blocking_demo")?;
    client.close()
}
