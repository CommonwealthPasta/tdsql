//! Connect once, then run a few statements over the same connection.
//!
//! ```text
//! docker run -e ACCEPT_EULA=Y -e SA_PASSWORD='YourStrong!Passw0rd' \
//!     -p 1433:1433 -d mcr.microsoft.com/mssql/server:2022-latest
//! cargo run --example basic
//! ```

use tdsql::{Client, Config};

#[tokio::main]
async fn main() -> tdsql::Result<()> {
    let mut client = Client::connect(
        &Config::new()
            .host("localhost")
            .port(1433)
            .database("master")
            .auth("sa", "YourStrong!Passw0rd")
            .trust_cert(),
    )
    .await?;

    // A single value.
    let version: String = client.query_scalar("SELECT @@VERSION", &[]).await?;
    println!(
        "connected to: {}",
        version.lines().next().unwrap_or_default()
    );

    // Positional parameters, and typed reads.
    let row = client
        .query_one("SELECT @P1 AS n, @P2 AS label", &[&42i32, &"answer"])
        .await?;
    let n: i32 = row.get("n");
    let label: String = row.get("label");
    println!("{label} = {n}");

    // Several rows.
    let rows = client
        .query(
            "SELECT name, database_id FROM sys.databases WHERE database_id <= @P1",
            &[&4i32],
        )
        .await?;

    for row in &rows {
        let name: String = row.get("name");
        let id: i32 = row.get("database_id");
        println!("  {id}: {name}");
    }

    // A nullable column reads as Option.
    let row = client
        .query_one("SELECT CAST(NULL AS int) AS v", &[])
        .await?;
    let v: Option<i32> = row.try_get("v")?;
    println!("null column: {v:?}");

    client.close().await
}
