//! Blocking-client integration tests.
//!
//! ```text
//! cargo test --features blocking --test blocking_test -- --ignored
//! ```

#![cfg(feature = "blocking")]

use tdsql::blocking::Client;
use tdsql::{Command, Config, Error, IsolationLevel};

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

fn ids(client: &mut Client, name: &str) -> Vec<i32> {
    client
        .query(&format!("SELECT id FROM dbo.{name} ORDER BY id"), &[])
        .unwrap()
        .iter()
        .map(|r| r.get::<i32, _>("id"))
        .collect()
}

fn drop_table(client: &mut Client, name: &str) {
    client
        .batch(&format!("DROP TABLE IF EXISTS dbo.{name}"))
        .unwrap();
}

// No #[tokio::test] here: these are ordinary synchronous tests, which is the
// entire point of the blocking client.
#[test]
#[ignore]
fn basic_query() {
    let mut client = connect();
    let rows = client.query("SELECT 1 AS value", &[]).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get::<i32, _>("value"), 1);
}

#[test]
#[ignore]
fn positional_params_and_typed_reads() {
    let mut client = connect();
    let row = client
        .query_one("SELECT @P1 AS n, @P2 AS s", &[&42i32, &"hello"])
        .unwrap();
    assert_eq!(row.get::<i32, _>("n"), 42);
    assert_eq!(row.get::<String, _>("s"), "hello");
}

#[test]
#[ignore]
fn scalar_and_execute() {
    let mut client = connect();
    let n: i32 = client.query_scalar("SELECT 7", &[]).unwrap();
    assert_eq!(n, 7);

    fresh_table(&mut client, "blk_exec");
    let affected = client
        .execute("INSERT INTO dbo.blk_exec (id) VALUES (1), (2), (3)", &[])
        .unwrap();
    assert_eq!(affected, 3);
    assert_eq!(ids(&mut client, "blk_exec"), vec![1, 2, 3]);
    drop_table(&mut client, "blk_exec");
}

#[test]
#[ignore]
fn query_one_rejects_wrong_row_counts() {
    let mut client = connect();
    let err = client
        .query_one("SELECT 1 AS v WHERE 1 = 0", &[])
        .unwrap_err();
    assert!(
        matches!(err, Error::UnexpectedRowCount { found: 0 }),
        "{err}"
    );
}

#[test]
#[ignore]
fn multiple_result_sets() {
    let mut client = connect();
    let ds = client
        .query_dataset(&Command::query("SELECT 1 AS a; SELECT 2 AS b"))
        .unwrap();
    assert_eq!(ds.len(), 2);
    assert_eq!(ds[0][0].get::<i32, _>("a"), 1);
    assert_eq!(ds[1][0].get::<i32, _>("b"), 2);
}

#[test]
#[ignore]
fn stored_procedure() {
    let mut client = connect();
    client
        .batch("IF OBJECT_ID('dbo.blk_sp', 'P') IS NOT NULL DROP PROCEDURE dbo.blk_sp")
        .unwrap();
    client
        .batch("CREATE PROCEDURE dbo.blk_sp @x INT AS BEGIN SELECT @x + 1 AS result; END")
        .unwrap();

    let ds = client
        .query_dataset(&Command::stored_procedure("dbo.blk_sp").param("x", 5))
        .unwrap();
    assert_eq!(ds[0][0].get::<i32, _>("result"), 6);

    client.batch("DROP PROCEDURE dbo.blk_sp").unwrap();
}

#[test]
#[ignore]
fn transaction_commits() {
    let mut client = connect();
    fresh_table(&mut client, "blk_tx_commit");

    let mut tx = client.transaction().unwrap();
    tx.execute("INSERT INTO dbo.blk_tx_commit (id) VALUES (@P1)", &[&1i32])
        .unwrap();
    tx.commit().unwrap();

    assert_eq!(ids(&mut client, "blk_tx_commit"), vec![1]);
    drop_table(&mut client, "blk_tx_commit");
}

/// The same guarantee as the async client: dropping never commits.
#[test]
#[ignore]
fn dropping_a_transaction_rolls_back() {
    let mut client = connect();
    fresh_table(&mut client, "blk_tx_drop");

    {
        let mut tx = client.transaction().unwrap();
        tx.execute("INSERT INTO dbo.blk_tx_drop (id) VALUES (@P1)", &[&1i32])
            .unwrap();
    }

    assert!(
        ids(&mut client, "blk_tx_drop").is_empty(),
        "dropping a blocking transaction must not commit it"
    );
    drop_table(&mut client, "blk_tx_drop");
}

#[test]
#[ignore]
fn error_path_rolls_back() {
    let mut client = connect();
    fresh_table(&mut client, "blk_tx_err");

    fn unit_of_work(client: &mut Client) -> tdsql::Result<()> {
        let mut tx = client.transaction()?;
        tx.execute("INSERT INTO dbo.blk_tx_err (id) VALUES (@P1)", &[&1i32])?;
        tx.execute("INSERT INTO dbo.blk_tx_err (id) VALUES (@P1)", &[&1i32])?;
        tx.commit()
    }

    assert!(unit_of_work(&mut client).is_err());
    assert!(ids(&mut client, "blk_tx_err").is_empty());
    drop_table(&mut client, "blk_tx_err");
}

#[test]
#[ignore]
fn savepoints_and_isolation() {
    let mut client = connect();
    fresh_table(&mut client, "blk_tx_sp");

    let mut tx = client
        .transaction_with_isolation(IsolationLevel::Serializable)
        .unwrap();
    tx.execute("INSERT INTO dbo.blk_tx_sp (id) VALUES (@P1)", &[&1i32])
        .unwrap();
    {
        let mut sp = tx.savepoint("inner").unwrap();
        sp.execute("INSERT INTO dbo.blk_tx_sp (id) VALUES (@P1)", &[&2i32])
            .unwrap();
        sp.rollback().unwrap();
    }
    tx.commit().unwrap();

    assert_eq!(ids(&mut client, "blk_tx_sp"), vec![1]);
    drop_table(&mut client, "blk_tx_sp");
}

#[test]
#[ignore]
fn client_survives_a_dropped_transaction() {
    let mut client = connect();
    {
        let mut tx = client.transaction().unwrap();
        tx.query_scalar::<i32>("SELECT 1", &[]).unwrap();
    }
    let n: i32 = client.query_scalar("SELECT 42", &[]).unwrap();
    assert_eq!(n, 42);
}

#[test]
#[ignore]
fn connects_from_a_connection_string() {
    let mut client = Client::connect_str(
        "Server=tcp:localhost,1433;Database=master;User Id=sa;\
         Password=YourStrong!Passw0rd;TrustServerCertificate=true",
    )
    .unwrap();
    let n: i32 = client.query_scalar("SELECT 1", &[]).unwrap();
    assert_eq!(n, 1);
}

/// Creating a blocking client inside an async runtime must fail cleanly rather
/// than panic later with "Cannot start a runtime from within a runtime".
#[tokio::test]
async fn refuses_to_run_inside_an_async_runtime() {
    let err = Client::connect(&test_config())
        .expect_err("should refuse to build a runtime inside a runtime");
    assert!(matches!(err, Error::BlockingInAsync), "{err}");
}
