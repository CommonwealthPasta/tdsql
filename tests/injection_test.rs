//! SQL injection tests.
//!
//! Bound values travel as typed RPC parameters, never as text spliced into the
//! statement, so a value containing SQL is data and stays data. These tests
//! push classic payloads through every binding path the crate offers and check
//! that nothing executes.
//!
//! Each payload names the table the test itself created, so if any binding path
//! ever did concatenate values into SQL, the test would destroy its own fixture
//! and fail loudly rather than quietly passing.
//!
//! ```text
//! cargo test --test injection_test -- --ignored
//! ```

use tdsql::{Client, Command, Config, Error};

/// Classic payloads, aimed at `table`. Each would be catastrophic if it were
/// concatenated into the statement rather than bound.
fn payloads(table: &str) -> Vec<String> {
    vec![
        format!("'; DROP TABLE dbo.{table}; --"),
        format!("1; DROP TABLE dbo.{table}"),
        format!("'); DROP TABLE dbo.{table}; --"),
        "x' UNION SELECT name FROM sys.tables --".to_string(),
        "' OR '1'='1".to_string(),
        "\" OR \"\"=\"".to_string(),
        "'; EXEC sp_who; --".to_string(),
        // The crate's own placeholder syntax, as data.
        "@P1".to_string(),
        // A non-ASCII quote, in case anything normalises Unicode.
        format!("\u{2019}; DROP TABLE dbo.{table}; --"),
        // A backslash, which some escaping schemes mishandle.
        format!("a\\'; DROP TABLE dbo.{table}; --"),
    ]
}

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
async fn fresh_target(client: &mut Client, table: &str) {
    client
        .batch(&format!(
            "IF OBJECT_ID('dbo.{table}', 'U') IS NOT NULL DROP TABLE dbo.{table};
             CREATE TABLE dbo.{table} (id INT PRIMARY KEY, note NVARCHAR(400) NULL)"
        ))
        .await
        .unwrap();
}

async fn target_exists(client: &mut Client, table: &str) -> bool {
    client
        .query_scalar::<i32>(
            "SELECT COUNT(*) FROM sys.tables WHERE name = @P1",
            &[&table],
        )
        .await
        .unwrap()
        == 1
}

async fn drop_target(client: &mut Client, table: &str) {
    client
        .batch(&format!("DROP TABLE IF EXISTS dbo.{table}"))
        .await
        .unwrap();
}

/// A payload bound as a positional parameter comes back byte-for-byte as data.
#[tokio::test]
#[ignore]
async fn positional_parameters_are_data_not_sql() {
    const T: &str = "inj_positional";
    let mut client = connect().await;
    fresh_target(&mut client, T).await;
    let payloads = payloads(T);

    for (i, payload) in payloads.iter().enumerate() {
        let id = i as i32;

        client
            .execute(
                &format!("INSERT INTO dbo.{T} (id, note) VALUES (@P1, @P2)"),
                &[&id, payload],
            )
            .await
            .unwrap();

        let back: String = client
            .query_scalar(&format!("SELECT note FROM dbo.{T} WHERE id = @P1"), &[&id])
            .await
            .unwrap();
        assert_eq!(&back, payload, "the payload round-tripped unchanged");

        assert!(
            target_exists(&mut client, T).await,
            "payload {payload:?} executed as SQL"
        );
    }

    let n: i32 = client
        .query_scalar(&format!("SELECT COUNT(*) FROM dbo.{T}"), &[])
        .await
        .unwrap();
    assert_eq!(n, payloads.len() as i32);

    drop_target(&mut client, T).await;
}

/// The classic tautology: bound as a parameter it is compared literally, so it
/// selects nothing rather than everything.
#[tokio::test]
#[ignore]
async fn a_tautology_payload_matches_nothing() {
    const T: &str = "inj_tautology";
    let mut client = connect().await;
    fresh_target(&mut client, T).await;

    client
        .execute(
            &format!("INSERT INTO dbo.{T} (id, note) VALUES (@P1, @P2)"),
            &[&1i32, &"real row"],
        )
        .await
        .unwrap();

    let rows = client
        .query(
            &format!("SELECT id FROM dbo.{T} WHERE note = @P1"),
            &[&"' OR '1'='1"],
        )
        .await
        .unwrap();
    assert!(
        rows.is_empty(),
        "the tautology should be compared as a string, not evaluated"
    );

    drop_target(&mut client, T).await;
}

/// The named-parameter `Command` path rewrites `@name` to `@P1` and then binds
/// the same way, so payloads stay data there too.
#[tokio::test]
#[ignore]
async fn named_command_parameters_are_data_not_sql() {
    const T: &str = "inj_named";
    let mut client = connect().await;
    fresh_target(&mut client, T).await;
    let payloads = payloads(T);

    for (i, payload) in payloads.iter().enumerate() {
        let cmd = Command::query(format!(
            "INSERT INTO dbo.{T} (id, note) VALUES (@id, @note)"
        ))
        .param("id", i as i32)
        .param("note", payload.clone());
        client.execute_command(&cmd).await.unwrap();

        assert!(
            target_exists(&mut client, T).await,
            "payload {payload:?} executed as SQL through Command"
        );
    }

    let stored: Vec<String> = client
        .query(&format!("SELECT note FROM dbo.{T} ORDER BY id"), &[])
        .await
        .unwrap()
        .iter()
        .map(|r| r.get::<String, _>("note"))
        .collect();
    assert_eq!(stored, payloads, "every payload stored verbatim");

    drop_target(&mut client, T).await;
}

/// Stored procedure arguments travel as genuinely named RPC parameters, and the
/// procedure name goes in the RPC's own name field rather than an `EXEC` string.
#[tokio::test]
#[ignore]
async fn stored_procedure_parameters_are_data_not_sql() {
    const T: &str = "inj_proc";
    const P: &str = "inj_proc_insert";
    let mut client = connect().await;
    fresh_target(&mut client, T).await;
    let payloads = payloads(T);

    client
        .batch(&format!("DROP PROCEDURE IF EXISTS dbo.{P}"))
        .await
        .unwrap();
    client
        .batch(&format!(
            "CREATE PROCEDURE dbo.{P} @id INT, @note NVARCHAR(400) AS
             BEGIN
                 SET NOCOUNT ON;
                 INSERT INTO dbo.{T} (id, note) VALUES (@id, @note);
             END"
        ))
        .await
        .unwrap();

    for (i, payload) in payloads.iter().enumerate() {
        let cmd = Command::stored_procedure(format!("dbo.{P}"))
            .param("id", i as i32)
            .param("note", payload.clone());
        client.execute_command(&cmd).await.unwrap();

        assert!(
            target_exists(&mut client, T).await,
            "payload {payload:?} executed as SQL through a stored procedure"
        );
    }

    let stored: Vec<String> = client
        .query(&format!("SELECT note FROM dbo.{T} ORDER BY id"), &[])
        .await
        .unwrap()
        .iter()
        .map(|r| r.get::<String, _>("note"))
        .collect();
    assert_eq!(stored, payloads, "every payload stored verbatim");

    client
        .batch(&format!("DROP PROCEDURE IF EXISTS dbo.{P}"))
        .await
        .unwrap();
    drop_target(&mut client, T).await;
}

/// Savepoint names *are* interpolated into `SAVE TRANSACTION [...]`, so they
/// are validated instead of trusted. Every payload must be rejected before it
/// reaches the server.
#[tokio::test]
#[ignore]
async fn savepoint_names_reject_every_payload() {
    let mut client = connect().await;

    let mut names = payloads("inj_nothing");
    // A `]` would otherwise close the bracket quoting in `SAVE TRANSACTION [..]`.
    names.push("a]; DROP TABLE dbo.inj_nothing; --".to_string());

    for name in &names {
        let mut tx = client.transaction().await.unwrap();
        let err = tx
            .savepoint(name.clone())
            .await
            .expect_err("a non-identifier savepoint name must be rejected");
        assert!(
            matches!(err, Error::InvalidSavepointName(_)),
            "name {name:?} gave {err}"
        );
        tx.rollback().await.unwrap();
    }
}
