//! Named parameters, stored procedures, and multiple result sets.
//!
//! ```text
//! cargo run --example stored_procedure
//! ```

use tdsql::{Client, Command, Config};

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

    // Named parameters in a text batch are rewritten to the positional form.
    let cmd = Command::query("SELECT @id AS id, @status AS status")
        .param("id", 1001)
        .param("status", "PAID");

    let ds = client.query_dataset(&cmd).await?;
    println!(
        "id={} status={}",
        ds[0][0].get::<i32, _>("id"),
        ds[0][0].get::<String, _>("status")
    );

    // Set up a procedure to call. CREATE PROCEDURE must start its own batch,
    // so it goes through `batch` rather than the parameterised path.
    client
        .batch(
            "IF OBJECT_ID('dbo.sp_tdsql_demo', 'P') IS NOT NULL \
             DROP PROCEDURE dbo.sp_tdsql_demo",
        )
        .await?;
    client
        .batch(
            "CREATE PROCEDURE dbo.sp_tdsql_demo @x INT AS BEGIN \
             SELECT @x AS given; SELECT @x * 2 AS doubled; END",
        )
        .await?;

    // Stored procedures go out as a real RPC, with genuinely named parameters.
    let cmd = Command::stored_procedure("dbo.sp_tdsql_demo").param("x", 21);
    let ds = client.query_dataset(&cmd).await?;

    // Two result sets, in the order the server sent them.
    println!("result sets: {}", ds.len());
    println!("  given:   {}", ds[0][0].get::<i32, _>("given"));
    println!("  doubled: {}", ds[1][0].get::<i32, _>("doubled"));

    client.batch("DROP PROCEDURE dbo.sp_tdsql_demo").await?;

    client.close().await
}
